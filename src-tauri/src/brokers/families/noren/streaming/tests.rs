//! The Noren feed's routing (FB-08), what a frame's tick carries for each
//! member (FB-07) and its data-silence watchdog (FB-03), from raw frames
//! through `NorenFeed::parse_text`.

use super::NorenFeed;
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, NormalizedTick, QuoteField,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::NorenConfig;
use crate::brokers::{flattrade, shoonya, tradesmart, zebu};
use std::sync::atomic::{AtomicI64, Ordering};

fn members() -> [&'static NorenConfig; 4] {
    [
        shoonya::config(),
        flattrade::config(),
        zebu::config(),
        tradesmart::config(),
    ]
}

fn feed(cfg: &'static NorenConfig) -> NorenFeed {
    NorenFeed::new(cfg, "wss://x/", "U1", "tok", SymbolResolver::new())
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: String::new(),
        brexchange: exchange.into(),
        mode,
        depth: 5,
    }
}

/// The one tick a frame gives.
fn tick(f: &mut NorenFeed, frame: &str) -> NormalizedTick {
    let ev = f.parse_text(frame);
    match ev.first() {
        Some(FeedEvent::Tick(t)) => t.clone(),
        other => panic!("{}: no tick from {}: {:?}", f.cfg.id, frame, other),
    }
}

/// Audit FB-08; web `<member>_adapter.py` `_resolve_scrip`: a frame
/// without `e` reaches the one subscribed scrip with its token, a token two
/// subscribed exchanges share is dropped, and a frame with `e` is routed
/// by it alone.
#[test]
fn a_frame_without_its_exchange_reaches_the_one_scrip_with_its_token() {
    for cfg in members() {
        let mut f = feed(cfg);
        f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Quote)]);
        let t = tick(&mut f, r#"{"t":"tk","tk":"2885","lp":"1412.30"}"#);
        assert_eq!(
            (t.symbol.as_str(), t.exchange.as_str(), t.ltp),
            ("RELIANCE", "NSE", 1412.3),
            "{}",
            cfg.id
        );
        let t = tick(&mut f, r#"{"t":"tf","e":"","tk":"2885","lp":"1412.35"}"#);
        assert_eq!(t.ltp, 1412.35, "{}: an empty e is no exchange", cfg.id);
        f.subscribe_frames(&[sub("USDINR25NOV26FUT", "CDS", "2885", FeedMode::Quote)]);
        assert!(
            f.parse_text(r#"{"t":"tf","tk":"2885","lp":"1412.40"}"#)
                .is_empty(),
            "{}: two scrips share the token",
            cfg.id
        );
        let t = tick(&mut f, r#"{"t":"tf","e":"CDS","tk":"2885","lp":"83.25"}"#);
        assert_eq!(t.symbol, "USDINR25NOV26FUT", "{}", cfg.id);
        assert!(
            f.parse_text(r#"{"t":"tf","e":"BSE","tk":"2885","lp":"1.00"}"#)
                .is_empty(),
            "{}: an exchange no scrip is on is not routed by the token",
            cfg.id
        );
    }
}

/// Audit FB-07; web `QuoteNormalizer` (`percent_change = safe_float(pc)`)
/// in every member: Noren's own `pc` is the change percent; without one
/// there is no change, never one worked out from the previous close
/// (lead rule no-derived-change).
#[test]
fn the_change_percent_is_noren_pc_and_absent_without_one() {
    for cfg in members() {
        let mut f = feed(cfg);
        f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Quote)]);
        let t = tick(
            &mut f,
            r#"{"t":"tk","e":"NSE","tk":"26000","lp":"25495.60","pc":"0.53","c":"25362.45"}"#,
        );
        assert_eq!((t.change, t.change_percent), (133.15, 0.53), "{}", cfg.id);
        assert!(t.knows(QuoteField::Change));

        let mut f = feed(cfg);
        f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Quote)]);
        let t = tick(
            &mut f,
            r#"{"t":"tk","e":"NSE","tk":"26000","lp":"25495.60","c":"25362.45"}"#,
        );
        assert!(
            !t.knows(QuoteField::Change),
            "{}: no pc, no change, though the close is known",
            cfg.id
        );
    }
}

/// Audit FB-07: `ft` is Noren's feed time (the web's `<member>_timestamp`),
/// the trade time is its `ltt`, a time of day in IST; one read just after
/// midnight belongs to the evening before.
#[test]
fn the_trade_time_is_noren_ltt_and_ft_is_the_feed_time() {
    let mut f = feed(shoonya::config());
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Quote)]);
    let t = tick(
        &mut f,
        r#"{"t":"tk","e":"NSE","tk":"2885","lp":"1412.30","ltt":"10:15:40","ft":"1794199542"}"#,
    );
    assert_eq!(t.last_trade_time_ms, 1_794_199_540_000);
    assert_eq!(t.extras.exchange_time_ms, Some(1_794_199_542_000));
    let t = tick(
        &mut f,
        r#"{"t":"tf","e":"NSE","tk":"2885","lp":"1412.30","ltt":"15:29:59","ft":"1794249030"}"#,
    );
    assert_eq!(t.last_trade_time_ms, 1_794_218_399_000);
    let mut f = feed(shoonya::config());
    f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Quote)]);
    let t = tick(
        &mut f,
        r#"{"t":"tk","e":"NSE","tk":"2885","lp":"1412.30","ltt":"--"}"#,
    );
    assert!(!t.knows(QuoteField::LastTradeTime));
    assert_eq!(t.extras.exchange_time_ms, None);
}

/// Noren's `tf` is a partial update: a key it leaves out has not changed
/// (docs/port/market-data-frames.md, "Partial-update feeds"). A new price
/// without `ft`, `ltt` or `pc` keeps the cached ones, as on the web, until
/// a packet brings its own; a new connection starts afresh.
#[test]
fn a_partial_update_keeps_the_times_and_change_it_left_out() {
    for cfg in members() {
        let mut f = feed(cfg);
        f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Quote)]);
        let t = tick(
            &mut f,
            r#"{"t":"tk","e":"NSE","tk":"2885","lp":"1412.30","c":"1403.00","pc":"0.66","ltt":"10:15:42","ft":"1794199542"}"#,
        );
        assert!(t.knows(QuoteField::LastTradeTime), "{}", cfg.id);
        assert_eq!(t.extras.exchange_time_ms, Some(1_794_199_542_000));
        let trade_time = t.last_trade_time_ms;

        let t = tick(&mut f, r#"{"t":"tf","e":"NSE","tk":"2885","lp":"1413.00"}"#);
        assert_eq!(t.ltp, 1413.0, "{}", cfg.id);
        assert_eq!(t.last_trade_time_ms, trade_time, "{}: trade time", cfg.id);
        assert_eq!(
            t.extras.exchange_time_ms,
            Some(1_794_199_542_000),
            "{}: feed time",
            cfg.id
        );
        assert_eq!(t.change_percent, 0.66, "{}: Noren's own change", cfg.id);

        let t = tick(
            &mut f,
            r#"{"t":"tf","e":"NSE","tk":"2885","lp":"1413.50","ltt":"10:15:44","ft":"1794199545"}"#,
        );
        assert_eq!(t.last_trade_time_ms, 1_794_199_544_000, "{}", cfg.id);
        assert_eq!(t.extras.exchange_time_ms, Some(1_794_199_545_000));

        f.on_connected();
        let t = tick(
            &mut f,
            r#"{"t":"tk","e":"NSE","tk":"2885","lp":"1414.00","c":"1403.00"}"#,
        );
        assert!(
            !t.knows(QuoteField::LastTradeTime),
            "{}: a new connection starts afresh",
            cfg.id
        );
        assert_eq!(t.extras.exchange_time_ms, None, "{}", cfg.id);
        assert!(!t.knows(QuoteField::Change), "{}", cfg.id);
    }
}

/// Audit FB-07; web DepthNormalizer / `_normalize_market_data`: Shoonya,
/// Flattrade and Zebu read `toi` in Depth, TradeSmart the native `oi`, in
/// Quote too. A depth packet is a mode 3 tick, a touchline packet mode 2.
#[test]
fn open_interest_is_read_as_each_members_web_adapter_reads_it() {
    let dk =
        r#"{"t":"dk","e":"NFO","tk":"59182","lp":"25480.00","oi":"12851625","toi":"12850050"}"#;
    let tk =
        r#"{"t":"tk","e":"NFO","tk":"59182","lp":"25480.00","oi":"12851625","toi":"12850050"}"#;
    for (cfg, oi, in_quote) in [
        (shoonya::config(), 12_850_050, false),
        (flattrade::config(), 12_850_050, false),
        (zebu::config(), 12_850_050, false),
        (tradesmart::config(), 12_851_625, true),
    ] {
        let mut f = feed(cfg);
        f.subscribe_frames(&[sub("NIFTY25NOV26FUT", "NFO", "59182", FeedMode::Depth)]);
        let t = tick(&mut f, dk);
        assert_eq!((t.mode, t.oi), (3, oi), "{}", cfg.id);
        assert!(t.knows(QuoteField::OpenInterest), "{}", cfg.id);
        let mut f = feed(cfg);
        f.subscribe_frames(&[sub("NIFTY25NOV26FUT", "NFO", "59182", FeedMode::Quote)]);
        let t = tick(&mut f, tk);
        assert_eq!(t.mode, 2, "{}", cfg.id);
        assert_eq!(t.knows(QuoteField::OpenInterest), in_quote, "{}", cfg.id);
    }
}

/// Web openalgo#2376 against the members' `safe_float` defaults: a key the
/// scrip's snapshot never held is not carried (an index touchline has no
/// volume, average price, last quantity or totals), circuits and the
/// 52-week range are carried as sent, and a book without totals says so.
#[test]
fn a_key_the_snapshot_never_held_is_not_carried() {
    let mut f = feed(flattrade::config());
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "26000", FeedMode::Quote)]);
    let t = tick(
        &mut f,
        r#"{"t":"tk","e":"NSE","tk":"26000","lp":"25495.60","o":"25380.10","h":"25510.75","l":"25340.20","c":"25362.45"}"#,
    );
    for absent in [
        QuoteField::Volume,
        QuoteField::AveragePrice,
        QuoteField::LastQuantity,
        QuoteField::TotalBuyQuantity,
        QuoteField::TotalSellQuantity,
    ] {
        assert!(!t.knows(absent), "{:?}", absent);
    }
    for known in [QuoteField::Open, QuoteField::High, QuoteField::Close] {
        assert!(t.knows(known), "{:?}", known);
    }
    assert!(t.extras.upper_circuit.is_none());

    let mut f = feed(zebu::config());
    f.subscribe_frames(&[sub("NIFTY25NOV26FUT", "NFO", "59182", FeedMode::Depth)]);
    let ev = f.parse_text(
        r#"{"t":"dk","e":"NFO","tk":"59182","lp":"25480.00","uc":"27902.60","lc":"22829.40","52h":"26277.35","52l":"21743.65","bp1":"25479.90","bq1":"150","bo1":"2"}"#,
    );
    let [FeedEvent::Tick(t), FeedEvent::Depth(d)] = ev.as_slice() else {
        panic!("{:?}", ev)
    };
    let x = t.extras;
    assert_eq!(
        (
            x.upper_circuit,
            x.lower_circuit,
            x.high_52_week,
            x.low_52_week
        ),
        (Some(27902.6), Some(22829.4), Some(26277.35), Some(21743.65))
    );
    assert!(d.absent.contains(QuoteField::TotalBuyQuantity));
    assert!(d.absent.contains(QuoteField::TotalSellQuantity));
    assert!(!d.absent.contains(QuoteField::Ltp));
    assert_eq!(
        (d.buy[0].price, d.buy[0].quantity, d.buy[0].orders),
        (25479.9, 150, 2)
    );
    // Only the slots Noren sent are levels, a slot sent as 0 included; the
    // web pads a slot that never came with safe_float's 0.
    assert_eq!(d.buy.len(), 1);
    assert!(d.sell.is_empty());
    let ev = f.parse_text(r#"{"t":"df","e":"NFO","tk":"59182","sp1":"0.00","sq1":"0","so1":"0"}"#);
    let [FeedEvent::Tick(_), FeedEvent::Depth(d)] = ev.as_slice() else {
        panic!("{:?}", ev)
    };
    assert_eq!((d.buy.len(), d.sell.len(), d.sell[0].price), (1, 1, 0.0));
}

static NOW: AtomicI64 = AtomicI64::new(0);

fn fake_now() -> i64 {
    NOW.load(Ordering::SeqCst)
}

fn advance(ms: i64) {
    NOW.fetch_add(ms, Ordering::SeqCst);
}

/// Audit FB-03; web `test/test_flattrade_data_silence_watchdog.py::
/// test_a_session_that_only_answers_heartbeats_is_recycled` and
/// `test_a_reconnect_starts_the_watchdog_disarmed`, for every member: after
/// two minutes of ticks only heartbeat acks come; the feed is called
/// stalled at the sixth 30 s check and not before, heartbeats never count
/// as data, and a new connection starts disarmed.
#[test]
fn a_session_that_only_answers_heartbeats_is_called_stalled() {
    for cfg in members() {
        NOW.store(1_700_000_000_000, Ordering::SeqCst);
        let mut f = feed(cfg);
        f.set_clock(fake_now);
        f.on_connected();
        f.subscribe_frames(&[sub("RELIANCE", "NSE", "2885", FeedMode::Quote)]);
        for _ in 0..24 {
            f.parse_text(r#"{"t":"tf","e":"NSE","tk":"2885","lp":"1412.30"}"#);
            advance(5_000);
        }
        let mut verdicts = Vec::new();
        for _ in 0..6 {
            advance(30_000);
            f.parse_text(r#"{"t":"hk"}"#);
            verdicts.push(f.data_stalled());
        }
        assert_eq!(
            verdicts,
            [false, false, false, false, false, true],
            "{}",
            cfg.id
        );
        f.on_connected();
        advance(10 * 180_000);
        assert!(
            !f.data_stalled(),
            "{}: a new connection is disarmed",
            cfg.id
        );
    }
}
