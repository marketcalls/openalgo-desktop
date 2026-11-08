//! The Noren feed's data-silence watchdog (FB-03), from raw frames through
//! `NorenFeed::parse_text`.

use super::NorenFeed;
use crate::brokers::common::streaming::{BrokerFeed, FeedMode, FeedSubscription};
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
