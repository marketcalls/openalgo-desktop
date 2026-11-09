use super::model::{self, OptionContract};
use super::ticker::{self, TICK};
use super::*;
use crate::brokers::common::streaming::{NormalizedDepth, NormalizedTick, QuoteField};
use crate::brokers::common::symbols::SymToken;
use crate::clock::ManualClock;
use chrono::TimeZone;
use futures_util::StreamExt;
use std::time::Duration;

/// 2026-11-09 09:15:00 IST.
const NOW_MS: i64 = 1_793_000_000_000;

fn call(strike: u32) -> SymToken {
    SymToken {
        symbol: format!("NIFTY25NOV26{}CE", strike),
        brsymbol: format!("NIFTY-Nov2026-{}-CE", strike),
        name: "NIFTY".into(),
        exchange: "NFO".into(),
        brexchange: "NSE_FNO".into(),
        token: strike.to_string(),
        expiry: "25-NOV-26".into(),
        strike: f64::from(strike),
        lot_size: 75,
        instrument_type: "CE".into(),
        tick_size: 0.05,
    }
}

/// NIFTY calls (and their puts) from 24,000 to 26,000, 50 apart: 41
/// strikes, median 25,000.
fn nifty_master() -> SymbolResolver {
    let symbols = SymbolResolver::new();
    let mut rows = Vec::new();
    for strike in (24_000..=26_000).step_by(50) {
        let c = call(strike);
        let mut p = c.clone();
        p.symbol = p.symbol.replace("CE", "PE");
        p.instrument_type = "PE".into();
        p.token = format!("{}P", strike);
        rows.push(c);
        rows.push(p);
    }
    symbols.load(rows);
    symbols
}

fn sub(symbol: &str, exchange: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: "1".into(),
        brsymbol: symbol.into(),
        brexchange: exchange.into(),
        mode,
        depth: 5,
    }
}

fn feed(seed: u64) -> DhanSandboxFeed {
    let clock = ManualClock::new(chrono::Utc.timestamp_millis_opt(NOW_MS).unwrap());
    DhanSandboxFeed::seeded("ws://127.0.0.1:9/unused", nifty_master(), seed).with_clock(clock)
}

fn tick_frame() -> Message {
    Message::Text(TICK.into())
}

fn one_tick(events: &[FeedEvent]) -> &NormalizedTick {
    match events {
        [FeedEvent::Tick(t), ..] => t,
        other => panic!("not a tick: {:?}", other),
    }
}

#[test]
fn an_option_symbol_is_read_as_the_web_reads_it() {
    let c = |u: &str, strike: f64, call: bool| {
        Some(OptionContract {
            underlying: u.into(),
            strike,
            call,
        })
    };
    assert_eq!(
        model::option_contract("NIFTY25NOV2625000CE"),
        c("NIFTY", 25_000.0, true)
    );
    assert_eq!(
        model::option_contract("vedl25apr26292.5pe"),
        c("VEDL", 292.5, false)
    );
    for not_one in [
        "RELIANCE",
        "NIFTY25NOV26FUT",
        "M&M25NOV263000CE",
        "NIFTY25XYZ2625000CE",
        "NIFTY25NOV26CE",
        "NIFTY25NOV261.2.3CE",
        "25NOV2625000CE",
    ] {
        assert_eq!(model::option_contract(not_one), None, "{}", not_one);
    }
}

/// Values from the web's own `_stable_noise` and `_derive_reference_price`
/// (run on the same master), so the starting prices are the web's.
#[test]
fn the_starting_price_is_the_webs() {
    assert!((model::stable_noise("x", 0.0, 1.0) - 0.17750681987439906).abs() < 1e-12);
    let symbols = nifty_master();
    let mut spots = model::SpotHints::default();
    for (symbol, web) in [
        ("RELIANCE", 324.75434554851483),
        ("sbin", 1089.2367763931948),
        ("NIFTY", 25_000.0),
        ("NIFTY25NOV2625000CE", 147.88869060817842),
        ("NIFTY25NOV2624000PE", 103.38557264797518),
        ("NIFTY25NOV2626500CE", 77.14898945424915),
        ("BANKNIFTY25NOV2655000CE", 334.0220469026645),
        ("VEDL25APR26292.5CE", 1.751974761324801),
        ("NIFTY25NOV26FUT", 407.2097744809487),
        ("M&M", 386.7606629400422),
    ] {
        let got = model::reference_price(symbol, &mut |u| spots.spot(&symbols, u));
        assert!(
            (got - web).abs() < 1e-9,
            "{}: {} against the web's {}",
            symbol,
            got,
            web
        );
    }
}

#[test]
fn a_second_moves_the_price_within_the_webs_step() {
    let mut rng = StdRng::seed_from_u64(7);
    let mut p = 324.75;
    for _ in 0..500 {
        let next = model::next_price(p, None, &mut rng);
        assert!((next - p).abs() <= (p * 0.0025).max(0.05) + 1e-9);
        p = next;
    }
    // An option drifts 18% of the way to its fair premium, plus noise.
    let fair = 150.0;
    let mut p = 100.0;
    for _ in 0..500 {
        let next = model::next_price(p, Some(fair), &mut rng);
        let noise = next - p - (fair - p) * 0.18;
        assert!(noise.abs() <= (fair * 0.02).max(0.2) + 1e-9 || next == 0.05);
        assert!(next >= 0.05);
        p = next;
    }
    assert!(
        (p - fair).abs() < 10.0,
        "the walk stays near its fair premium"
    );
    assert!(
        model::next_price(0.05, Some(0.5), &mut rng) >= 0.05,
        "never below 0.05"
    );
}

#[test]
fn an_ltp_second_carries_the_price_and_its_time() {
    let mut f = feed(1);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Ltp)]);
    let ev = f.parse(&tick_frame());
    assert_eq!(ev.len(), 1);
    let t = one_tick(&ev);
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("RELIANCE", "NSE", 1)
    );
    assert_eq!((t.timestamp_ms, t.last_trade_time_ms), (NOW_MS, NOW_MS));
    assert!(
        (t.ltp - 324.75).abs() <= 0.82,
        "one step from the starting price: {}",
        t.ltp
    );
    assert!(t.knows(QuoteField::Ltp) && t.knows(QuoteField::LastTradeTime));
    assert!(!t.knows(QuoteField::Volume) && !t.knows(QuoteField::Change));
}

#[test]
fn a_quote_second_carries_the_webs_fields() {
    let mut f = feed(2);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    for _ in 0..50 {
        let ev = f.parse(&tick_frame());
        let t = one_tick(&ev);
        assert_eq!(t.mode, 2);
        assert_eq!(t.average_price, t.ltp);
        for (got, factor) in [
            (t.open, 0.95),
            (t.high, 1.05),
            (t.low, 0.90),
            (t.close, 0.98),
        ] {
            assert!(
                (got - t.ltp * factor).abs() <= 0.011,
                "{} against {} x {}",
                got,
                t.ltp,
                factor
            );
        }
        assert!((1000..=50_000).contains(&t.volume));
        assert!((10_000..=500_000).contains(&t.oi));
        assert!((1..=100).contains(&t.last_quantity));
        assert!((5000..=100_000).contains(&t.total_buy_quantity));
        assert!((5000..=100_000).contains(&t.total_sell_quantity));
        assert!(
            t.knows(QuoteField::OpenInterest),
            "the web's Quote carries OI"
        );
        assert!(
            !t.knows(QuoteField::Change),
            "the web's frame has no change"
        );
    }
}

#[test]
fn a_depth_second_carries_five_levels_a_side_around_the_price() {
    let mut f = feed(3);
    f.subscribe_frames(&[sub("NIFTY25NOV2625000CE", "NFO", FeedMode::Depth)]);
    let ev = f.parse(&tick_frame());
    let (t, d): (&NormalizedTick, &NormalizedDepth) = match &ev[..] {
        [FeedEvent::Tick(t), FeedEvent::Depth(d)] => (t, d),
        other => panic!("{:?}", other),
    };
    assert_eq!(t.mode, 3);
    assert_eq!((d.ltp, d.timestamp_ms), (t.ltp, NOW_MS));
    assert_eq!(
        (d.total_buy_quantity, d.total_sell_quantity),
        (t.total_buy_quantity, t.total_sell_quantity)
    );
    assert_eq!((d.buy.len(), d.sell.len()), (5, 5));
    for (i, (b, s)) in d.buy.iter().zip(&d.sell).enumerate() {
        let gap = 0.05 * (i + 1) as f64;
        assert!(
            (t.ltp - gap - b.price).abs() <= 0.011,
            "bid {} at {}",
            b.price,
            i
        );
        assert!(
            (t.ltp + gap - s.price).abs() <= 0.011,
            "ask {} at {}",
            s.price,
            i
        );
        for l in [b, s] {
            assert!((10..=500).contains(&l.quantity) && (1..=10).contains(&l.orders));
        }
    }
}

#[test]
fn one_subscription_per_instrument_at_the_mode_asked_last() {
    let mut f = feed(4);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Depth)]);
    assert_eq!(f.parse(&tick_frame()).len(), 2, "one Depth subscription");
    f.unsubscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Depth)]);
    assert!(f.parse(&tick_frame()).is_empty());
    // The price stays: subscribing again goes on from it.
    let kept = f.prices["NSE_RELIANCE"];
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Ltp)]);
    assert_eq!(f.prices["NSE_RELIANCE"], kept);
    let t = f.parse(&tick_frame());
    assert!((one_tick(&t).ltp - kept).abs() <= (kept * 0.0025).max(0.05) + 0.006);
}

#[test]
fn the_same_seed_gives_the_same_seconds() {
    let run = |seed| {
        let mut f = feed(seed);
        f.subscribe_frames(&[
            sub("RELIANCE", "NSE", FeedMode::Depth),
            sub("NIFTY25NOV2624000PE", "NFO", FeedMode::Quote),
        ]);
        (0..5)
            .flat_map(|_| f.parse(&tick_frame()))
            .collect::<Vec<_>>()
    };
    assert_eq!(run(9), run(9));
    assert_ne!(run(9), run(10));
}

#[test]
fn a_reconnect_waits_for_the_subscriptions_again() {
    let mut f = feed(5);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    assert!(f.on_connected().is_empty());
    assert!(f.parse(&tick_frame()).is_empty());
    assert!(f
        .subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)])
        .is_empty());
    assert_eq!(f.parse(&tick_frame()).len(), 1);
}

#[test]
fn nothing_but_the_tick_frame_makes_a_second() {
    let mut f = feed(6);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    for m in [
        Message::Pong(Vec::new()),
        Message::Text(r#"{"openalgo_sim":"tock"}"#.into()),
        Message::Binary(TICK.as_bytes().to_vec()),
    ] {
        assert!(f.parse(&m).is_empty(), "{:?}", m);
    }
}

async fn next_text<S>(rx: &mut S) -> Option<String>
where
    S: futures_util::Stream<
            Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    match tokio::time::timeout(Duration::from_secs(3), rx.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => Some(t.to_string()),
        _ => None,
    }
}

#[tokio::test]
async fn the_ticker_sends_one_tick_a_second_on_its_secret_path() {
    let slot = parking_lot::Mutex::new(None);
    let url = ticker::ensure_started(&slot).expect("a local port");
    assert_eq!(
        ticker::ensure_started(&slot).expect("running"),
        url,
        "one ticker"
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("the secret path");
    assert_eq!(next_text(&mut ws).await.as_deref(), Some(TICK));
    assert_eq!(next_text(&mut ws).await.as_deref(), Some(TICK));
    let (base, _) = url.rsplit_once('/').expect("a path");
    assert!(
        tokio_tungstenite::connect_async(format!("{}/wrong", base))
            .await
            .is_err(),
        "another path is refused"
    );
}

#[tokio::test]
async fn a_new_connection_replaces_the_old_and_dropping_the_ticker_closes_it() {
    let slot = parking_lot::Mutex::new(None);
    let url = ticker::ensure_started(&slot).expect("a local port");
    let (mut first, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("first");
    assert_eq!(next_text(&mut first).await.as_deref(), Some(TICK));
    let (mut second, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("second");
    assert_eq!(next_text(&mut second).await.as_deref(), Some(TICK));
    assert_eq!(next_text(&mut first).await, None, "the first is closed");
    drop(slot.lock().take());
    assert_eq!(next_text(&mut second).await, None, "closed with the ticker");
    assert!(tokio_tungstenite::connect_async(url.as_str())
        .await
        .is_err());
}
