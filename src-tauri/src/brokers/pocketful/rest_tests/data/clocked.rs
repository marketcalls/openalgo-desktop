//! Data cases that need a fixed IST date or a short deadline: today's daily
//! bar from a quote (web `test/test_pocketful_data.py`, marketcalls/openalgo#2467,
//! a1d539764, which fixes `_ist_today`) and the one deadline a history or a
//! batch of quotes has (desktop: the web has none).

use super::history::{candles, day, jan, req, HISTORICAL, JAN1_UTC_MIDNIGHT};
use super::{market_data_with, master, plain_sentence, session, Reply};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::pocketful::data::{quote_each, UNANSWERED};
use crate::brokers::pocketful::history::history_on;
use crate::brokers::pocketful::PocketfulBroker;
use crate::brokers::types::*;
use crate::error::AppError;
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;

const NFO_QUOTE: &str = "/api/v1/marketdata/NFO/FutOpt";
/// Long enough for any answer the fake gives.
const AMPLE: Duration = Duration::from_secs(60);

/// Web `_daily_answer`: one daily candle per day.
fn daily_answer(days: &[&str]) -> Reply {
    candles(
        days.iter()
            .map(|d| json!([format!("{}T00:00:00+0530", d), 100, 110, 90, 105, 1000, 50]))
            .collect(),
    )
}

/// Web `_quote_traded_at`: the documented answer, last traded at `epoch`.
fn quote_traded_at(epoch: i64) -> Reply {
    Reply::Json(
        200,
        market_data_with(json!({
            "last_trade_time": epoch,
            "open_price": 200,
            "high_price": 210,
            "low_price": 195,
            "last_trade_price": 205,
            "trade_volume": 777,
            "current_open_interest": 60,
        })),
    )
}

fn future_daily() -> HistoryRequest {
    req("BANKNIFTY29JAN25FUT", "NFO", "D", jan(1), jan(2))
}

fn paths(fake: &super::Fake) -> Vec<String> {
    fake.seen().into_iter().map(|s| s.path).collect()
}

/// Web test_pocketful_data.py::test_todays_daily_bar_comes_from_the_quote_when_the_series_stops_before_it
#[tokio::test]
async fn todays_daily_bar_comes_from_the_quote_when_the_series_stops_before_it() {
    let jan2_1000_ist = JAN1_UTC_MIDNIGHT + 86_400 + 10 * 3_600 - 19_800;
    let fake = super::Fake::start().await;
    fake.on(HISTORICAL, vec![daily_answer(&["2025-01-01"])]);
    fake.on(NFO_QUOTE, vec![quote_traded_at(jan2_1000_ist)]);
    let b = fake.broker(master());

    let bars = history_on(&b, &session(), &future_daily(), jan(2), AMPLE)
        .await
        .unwrap();

    assert_eq!(paths(&fake), [HISTORICAL, NFO_QUOTE]);
    let times: Vec<i64> = bars.iter().map(|c| c.timestamp).collect();
    assert_eq!(times, [JAN1_UTC_MIDNIGHT, JAN1_UTC_MIDNIGHT + 86_400]);
    assert_eq!(
        bars[1],
        Candle {
            timestamp: JAN1_UTC_MIDNIGHT + 86_400,
            open: 200.0,
            high: 210.0,
            low: 195.0,
            close: 205.0,
            volume: 777,
            oi: 60,
        }
    );
}

/// Web test_pocketful_data.py::test_a_quote_from_an_earlier_session_adds_no_daily_bar
#[tokio::test]
async fn a_quote_from_an_earlier_session_adds_no_daily_bar() {
    let jan1_1500_ist = JAN1_UTC_MIDNIGHT + 15 * 3_600 - 19_800;
    let fake = super::Fake::start().await;
    fake.on(HISTORICAL, vec![daily_answer(&["2025-01-01"])]);
    fake.on(NFO_QUOTE, vec![quote_traded_at(jan1_1500_ist)]);
    let b = fake.broker(master());

    let bars = history_on(&b, &session(), &future_daily(), jan(2), AMPLE)
        .await
        .unwrap();

    let times: Vec<i64> = bars.iter().map(|c| c.timestamp).collect();
    assert_eq!(times, [JAN1_UTC_MIDNIGHT]);
}

/// Web test_pocketful_data.py::test_a_daily_series_that_reaches_today_asks_for_no_quote
#[tokio::test]
async fn a_daily_series_that_reaches_today_asks_for_no_quote() {
    let fake = super::Fake::start().await;
    fake.on(
        HISTORICAL,
        vec![daily_answer(&["2025-01-01", "2025-01-02"])],
    );
    let b = fake.broker(master());

    history_on(&b, &session(), &future_daily(), jan(2), AMPLE)
        .await
        .unwrap();

    assert_eq!(paths(&fake), [HISTORICAL]);
}

/// A host that takes connections and never answers: a broker that stopped
/// answering. The task is aborted when the guard goes.
struct Silent {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Silent {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        Self { base, task }
    }

    fn broker(&self, symbols: SymbolResolver) -> PocketfulBroker {
        PocketfulBroker::with_base_url(symbols, &self.base, "ws://127.0.0.1:9", &self.base)
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// CLAUDE.md, broker request limits: one history has one deadline (no web
/// case: the web waits as long as its requests do). A broker that stops
/// answering fails the history with a sentence well before a request's
/// own 30 s timeout.
#[tokio::test]
async fn a_history_past_its_deadline_fails_with_a_sentence() {
    let host = Silent::start().await;
    let b = host.broker(master());
    let started = std::time::Instant::now();

    let e = history_on(
        &b,
        &session(),
        &req("IDEA", "NSE", "1m", jan(1), jan(1)),
        day(2026, 11, 25),
        Duration::from_millis(300),
    )
    .await
    .unwrap_err();

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(e, AppError::Broker(_)), "{:?}", e);
    assert!(e.client_message().contains("IDEA on NSE"), "{}", e);
    plain_sentence(&e.client_message());
}

/// One batch of quotes has one deadline too (desktop): the symbols it
/// leaves unasked say so, never a quote of zeros, and a chain counts their
/// open interest as missing.
#[tokio::test]
async fn a_batch_past_its_deadline_answers_each_symbol_left_unasked() {
    let host = Silent::start().await;
    let b = host.broker(master());
    let keys = [
        QuoteKey::new("NSE", "IDEA"),
        QuoteKey::new("NSE", "RELIANCE"),
    ];
    let started = std::time::Instant::now();

    let (quotes, oi_missing) = quote_each(&b, &session(), &keys, Duration::from_millis(300))
        .await
        .unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let shape: Vec<(&str, bool, Option<&str>)> = quotes
        .iter()
        .map(|q| (q.symbol.as_str(), q.data.is_some(), q.error.as_deref()))
        .collect();
    assert_eq!(
        shape,
        [
            ("IDEA", false, Some(UNANSWERED)),
            ("RELIANCE", false, Some(UNANSWERED)),
        ]
    );
    plain_sentence(UNANSWERED);
    assert_eq!(
        oi_missing,
        HashSet::from(["IDEA".to_string(), "RELIANCE".to_string()])
    );
}
