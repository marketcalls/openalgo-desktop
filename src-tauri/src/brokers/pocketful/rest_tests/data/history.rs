//! History over REST (web `test/test_pocketful_data.py`, its chart and NFO
//! history cases, marketcalls/openalgo#2467, a1d539764), through the
//! `Broker` trait alone, against the candles printed on chart-data.md and
//! getting-started.md. Cases that need a fixed IST date or a short
//! deadline are in `clocked.rs`.

use super::super::{Fake, Reply, CLIENT};
use super::{
    chart_answer, error_answer, historical_answer, master, plain_sentence, session, unauthorised,
    ACCESS,
};
use crate::brokers::pocketful::PocketfulBroker;
use crate::brokers::types::*;
use crate::brokers::Broker;
use crate::error::AppError;
use crate::services::schemas::SUPPORTED_INTERVALS;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(super) const CHARTS: &str = "/api/v1/charts";
pub(super) const HISTORICAL: &str = "/api/data/v1/historical";
/// 2025-01-01T09:15:00+05:30 in epoch seconds.
pub(super) const JAN1_0915_IST: i64 = 1_735_703_100;
/// 2025-01-01 00:00 UTC in epoch seconds: a daily bar's time.
pub(super) const JAN1_UTC_MIDNIGHT: i64 = 1_735_689_600;

pub(super) fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

pub(super) fn jan(d: u32) -> NaiveDate {
    day(2025, 1, d)
}

pub(super) fn req(
    symbol: &str,
    exchange: &str,
    interval: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> HistoryRequest {
    HistoryRequest {
        key: QuoteKey::new(exchange, symbol),
        interval: interval.into(),
        start,
        end,
    }
}

/// Web `EMPTY_CANDLES`.
pub(super) fn empty_candles() -> Reply {
    candles(Vec::new())
}

/// Web `_candles`: a success carrying `rows`.
pub(super) fn candles(rows: Vec<Value>) -> Reply {
    Reply::Json(200, json!({"status": "success", "data": {"candles": rows}}))
}

/// Web `_candle`: a one-minute candle around `price`.
fn candle(stamp: &str, price: f64, volume: i64) -> Value {
    json!([stamp, price, price + 1.0, price - 1.0, price + 0.5, volume])
}

fn bar(timestamp: i64, ohlc: [f64; 4], volume: i64, oi: i64) -> Candle {
    Candle {
        timestamp,
        open: ohlc[0],
        high: ohlc[1],
        low: ohlc[2],
        close: ohlc[3],
        volume,
        oi,
    }
}

fn paths(fake: &Fake) -> Vec<String> {
    fake.seen().into_iter().map(|s| s.path).collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Web test_pocketful_data.py::test_every_advertised_interval_is_one_the_history_api_accepts,
/// with the capability that offers history at all. Bars stay off the
/// session grid: built bars start at the session open already, and the
/// grid's rebuild would ask for the minutes again and drop the NFO bars'
/// open interest.
#[test]
fn every_advertised_interval_is_one_the_history_api_accepts() {
    let b = PocketfulBroker::new(master());

    let c = b.capabilities();
    assert!(c.history, "history is offered");
    assert!(!c.multiquotes_batch, "one request per symbol");
    let map = b.timeframe_map();
    for (interval, _) in map {
        assert!(SUPPORTED_INTERVALS.contains(interval), "{}", interval);
    }
    // The values are the documented /api/data/v1/historical interval enum.
    let values: HashSet<&str> = map.iter().map(|(_, v)| *v).collect();
    assert_eq!(
        values,
        HashSet::from([
            "1minute", "3minute", "5minute", "10minute", "15minute", "30minute", "60minute",
            "1day",
        ])
    );
    for (interval, _) in map {
        let r = req("IDEA", "NSE", interval, jan(1), jan(2));
        assert!(!b.history_on_session_grid(&r), "{}", interval);
    }
}

/// Web test_pocketful_data.py::test_chart_history_asks_for_one_minute_candles_in_contiguous_chunks
#[tokio::test]
async fn chart_history_asks_for_one_minute_candles_in_contiguous_chunks() {
    let fake = Fake::start().await;
    fake.on(
        CHARTS,
        vec![Reply::Json(200, chart_answer()), empty_candles()],
    );
    let b = fake.broker(master());

    b.get_history(&session(), &req("IDEA", "NSE", "5m", jan(1), jan(20)))
        .await
        .unwrap();

    let seen = fake.seen();
    for s in &seen {
        assert_eq!((s.method.as_str(), s.path.as_str()), ("GET", CHARTS));
        // chart-data.md lists a clientid header beside the session's own.
        assert_eq!(s.header("clientid"), CLIENT);
        assert_eq!(s.header("authorization"), format!("Bearer {}", ACCESS));
    }
    let queries: Vec<Vec<(String, String)>> = seen.iter().map(|s| s.query_pairs()).collect();
    let expected: Vec<Vec<(String, String)>> = [
        ("2025-01-01T00:00:00", "2025-01-07T23:59:59"),
        ("2025-01-08T00:00:00", "2025-01-14T23:59:59"),
        ("2025-01-15T00:00:00", "2025-01-20T23:59:59"),
    ]
    .iter()
    .map(|(first, last)| {
        pairs(&[
            ("exchange", "NSE"),
            ("token", "14366"),
            ("candletype", "1"),
            ("from_date", first),
            ("to_date", last),
        ])
    })
    .collect();
    assert_eq!(queries, expected);
}

/// Web test_pocketful_data.py::test_one_minute_bars_are_the_documented_candles_at_their_utc_instant
#[tokio::test]
async fn one_minute_bars_are_the_documented_candles_at_their_utc_instant() {
    let fake = Fake::start().await;
    fake.on(CHARTS, vec![Reply::Json(200, chart_answer())]);
    let b = fake.broker(master());

    let bars = b
        .get_history(&session(), &req("IDEA", "NSE", "1m", jan(1), jan(1)))
        .await
        .unwrap();

    assert_eq!(
        bars,
        [
            bar(JAN1_0915_IST, [138.35, 138.35, 137.75, 137.85], 16699, 0),
            bar(JAN1_0915_IST + 60, [137.85, 137.85, 137.4, 137.7], 19642, 0),
        ]
    );
}

/// Web test_pocketful_data.py::test_built_bars_start_at_the_session_open
#[tokio::test]
async fn built_bars_start_at_the_session_open() {
    let cases = [
        // NSE opens at 09:15: ten one minute candles make two 5 minute bars.
        ("IDEA", "NSE", "5m", 9 * 60 + 15, 10, ["09:15", "09:20"], 5),
        ("IDEA", "NSE", "1h", 9 * 60 + 15, 70, ["09:15", "10:15"], 60),
        (
            "IDEA",
            "NSE",
            "10m",
            9 * 60 + 15,
            20,
            ["09:15", "09:25"],
            10,
        ),
        // MCX opens at 09:00.
        (
            "CRUDEOIL19DEC25FUT",
            "MCX",
            "1h",
            9 * 60,
            90,
            ["09:00", "10:00"],
            60,
        ),
    ];
    for (symbol, exchange, interval, first, count, starts, per_bar) in cases {
        let rows = (0..count)
            .map(|i: i64| {
                let at = first + i;
                let stamp = format!("2025-01-01T{:02}:{:02}:00+0530", at / 60, at % 60);
                candle(&stamp, 100.0 + i as f64, 100)
            })
            .collect();
        let fake = Fake::start().await;
        fake.on(CHARTS, vec![candles(rows)]);
        let b = fake.broker(master());

        let bars = b
            .get_history(&session(), &req(symbol, exchange, interval, jan(1), jan(1)))
            .await
            .unwrap();

        let epoch = |hhmm: &str| {
            let (h, m) = hhmm.split_once(':').unwrap();
            let minutes: i64 = h.parse::<i64>().unwrap() * 60 + m.parse::<i64>().unwrap();
            JAN1_UTC_MIDNIGHT + minutes * 60 - 19_800
        };
        let times: Vec<i64> = bars.iter().map(|c| c.timestamp).collect();
        let expected: Vec<i64> = starts.iter().map(|&s| epoch(s)).collect();
        assert_eq!(times, expected, "{} {}", exchange, interval);
        // The first bar is full: per_bar minutes of candles.
        let first_bar = bars[0];
        let last_price = 100.0 + (per_bar - 1) as f64;
        assert_eq!(first_bar.open, 100.0, "{}", interval);
        assert_eq!(first_bar.high, last_price + 1.0, "{}", interval);
        assert_eq!(first_bar.low, 99.0, "{}", interval);
        assert_eq!(first_bar.close, last_price + 0.5, "{}", interval);
        assert_eq!(first_bar.volume, 100 * per_bar, "{}", interval);
    }
}

/// Web test_pocketful_data.py::test_daily_bars_are_built_per_ist_date_at_utc_midnight
#[tokio::test]
async fn daily_bars_are_built_per_ist_date_at_utc_midnight() {
    let fake = Fake::start().await;
    fake.on(
        CHARTS,
        vec![candles(vec![
            candle("2025-01-01T09:15:00+0530", 100.0, 10),
            candle("2025-01-01T15:29:00+0530", 110.0, 20),
            candle("2025-01-02T09:15:00+0530", 120.0, 30),
        ])],
    );
    let b = fake.broker(master());

    let bars = b
        .get_history(&session(), &req("IDEA", "NSE", "D", jan(1), jan(2)))
        .await
        .unwrap();

    assert_eq!(
        bars,
        [
            bar(JAN1_UTC_MIDNIGHT, [100.0, 111.0, 99.0, 110.5], 30, 0),
            bar(
                JAN1_UTC_MIDNIGHT + 86_400,
                [120.0, 121.0, 119.0, 120.5],
                30,
                0
            ),
        ]
    );
}

/// A daily history over several chunks (no web test: the web builds the
/// bars once all chunks are in; the desktop builds each chunk's as it
/// arrives): one bar a day, oldest first, across the chunk boundary.
#[tokio::test]
async fn daily_bars_span_chunks_in_date_order() {
    let fake = Fake::start().await;
    fake.on(
        CHARTS,
        vec![
            candles(vec![
                candle("2025-01-07T09:15:00+0530", 107.0, 7),
                candle("2025-01-01T09:15:00+0530", 101.0, 1),
                candle("2025-01-07T09:16:00+0530", 108.0, 8),
            ]),
            candles(vec![candle("2025-01-08T09:15:00+0530", 110.0, 10)]),
        ],
    );
    let b = fake.broker(master());

    let bars = b
        .get_history(&session(), &req("IDEA", "NSE", "D", jan(1), jan(8)))
        .await
        .unwrap();

    assert_eq!(fake.seen().len(), 2, "two chunks");
    assert_eq!(
        bars,
        [
            bar(JAN1_UTC_MIDNIGHT, [101.0, 102.0, 100.0, 101.5], 1, 0),
            bar(
                JAN1_UTC_MIDNIGHT + 6 * 86_400,
                [107.0, 109.0, 106.0, 108.5],
                15,
                0
            ),
            bar(
                JAN1_UTC_MIDNIGHT + 7 * 86_400,
                [110.0, 111.0, 109.0, 110.5],
                10,
                0
            ),
        ]
    );
}

/// Web test_pocketful_data.py::test_candles_outside_the_requested_dates_are_dropped
#[tokio::test]
async fn candles_outside_the_requested_dates_are_dropped() {
    let fake = Fake::start().await;
    fake.on(
        CHARTS,
        vec![candles(vec![
            candle("2024-12-31T15:29:00+0530", 90.0, 100),
            candle("2025-01-01T09:15:00+0530", 100.0, 100),
            candle("2025-01-02T09:15:00+0530", 110.0, 100),
        ])],
    );
    let b = fake.broker(master());

    let bars = b
        .get_history(&session(), &req("IDEA", "NSE", "1m", jan(1), jan(1)))
        .await
        .unwrap();

    let times: Vec<i64> = bars.iter().map(|c| c.timestamp).collect();
    assert_eq!(times, [JAN1_0915_IST]);
}

/// Web test_pocketful_data.py::test_index_history_is_asked_on_the_cash_exchange
#[tokio::test]
async fn index_history_is_asked_on_the_cash_exchange() {
    let fake = Fake::start().await;
    fake.on(CHARTS, vec![Reply::Json(200, chart_answer())]);
    let b = fake.broker(master());

    b.get_history(&session(), &req("NIFTY", "NSE_INDEX", "1m", jan(1), jan(1)))
        .await
        .unwrap();

    let query = fake.seen()[0].query_pairs();
    assert!(
        query.contains(&("exchange".into(), "NSE".into())),
        "{:?}",
        query
    );
    assert!(
        query.contains(&("token".into(), "26000".into())),
        "{:?}",
        query
    );
}

/// Web test_pocketful_data.py::test_a_failed_chunk_fails_the_request_instead_of_leaving_a_gap
#[tokio::test]
async fn a_failed_chunk_fails_the_request_instead_of_leaving_a_gap() {
    let fake = Fake::start().await;
    fake.on(
        CHARTS,
        vec![
            Reply::Json(200, chart_answer()),
            Reply::Json(200, error_answer()),
            Reply::Json(200, chart_answer()),
        ],
    );
    let b = fake.broker(master());

    let e = b
        .get_history(&session(), &req("IDEA", "NSE", "1m", jan(1), jan(20)))
        .await
        .unwrap_err();

    assert_eq!(fake.seen().len(), 2);
    plain_sentence(&e.client_message());
}

/// Web test_pocketful_data.py::test_no_candles_is_an_empty_frame_with_the_columns
#[tokio::test]
async fn no_candles_is_no_bars() {
    let fake = Fake::start().await;
    fake.on(CHARTS, vec![empty_candles()]);
    let b = fake.broker(master());

    let bars = b
        .get_history(&session(), &req("IDEA", "NSE", "D", jan(1), jan(3)))
        .await
        .unwrap();

    assert!(bars.is_empty());
    assert_eq!(fake.seen().len(), 1, "it was asked");
}

/// Web test_pocketful_data.py::test_what_cannot_be_served_is_refused_before_any_request,
/// and a range longer than ten years (desktop: Pocketful's 10,000 data
/// requests a day are what a MARKET order's price comes from).
#[tokio::test]
async fn what_cannot_be_served_is_refused_before_any_request() {
    let fake = Fake::start().await;
    fake.on(CHARTS, vec![Reply::Json(200, chart_answer())]);
    let b = fake.broker(master());

    for r in [
        req("IDEA", "NSE", "2h", jan(1), jan(2)),
        req("IDEA", "NSE", "W", jan(1), jan(2)),
        req("NOPE", "NSE", "1m", jan(1), jan(2)),
        req("X", "NCDEX", "1m", jan(1), jan(2)),
        req("GUARSEED", "NCDEX", "1m", jan(1), jan(2)),
        req("IDEA", "NSE", "D", day(2014, 1, 1), jan(1)),
    ] {
        let e = b.get_history(&session(), &r).await.unwrap_err();
        plain_sentence(&e.client_message());
    }
    assert!(fake.seen().is_empty(), "nothing was asked for");
}

/// Web test_pocketful_data.py::test_nfo_option_history_posts_the_documented_body_and_keeps_oi
#[tokio::test]
async fn nfo_option_history_posts_the_documented_body_and_keeps_oi() {
    let fake = Fake::start().await;
    fake.on(HISTORICAL, vec![Reply::Json(200, historical_answer())]);
    let b = fake.broker(master());

    let bars = b
        .get_history(
            &session(),
            &req("BANKNIFTY29JAN2551000CE", "NFO", "1m", jan(1), jan(1)),
        )
        .await
        .unwrap();

    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("POST", HISTORICAL)
    );
    assert_eq!(
        seen[0].json(),
        json!({
            "symbol": "BANKNIFTY",
            "product_type": "options",
            "type": "CE",
            "strike_price": 51000,
            "expiry_date": "2025-01-29",
            "interval": "1minute",
            "from_date": "2025-01-01T00:00:00",
            "to_date": "2025-01-01T23:59:59",
        })
    );
    // The documented body sends a whole strike as 51000, not 51000.0.
    assert_eq!(seen[0].json()["strike_price"].to_string(), "51000");
    let times: Vec<i64> = bars.iter().map(|c| c.timestamp).collect();
    assert_eq!(times, [JAN1_0915_IST, JAN1_0915_IST + 60]);
    let oi: Vec<i64> = bars.iter().map(|c| c.oi).collect();
    assert_eq!(oi, [8805, 16725]);
    let close: Vec<f64> = bars.iter().map(|c| c.close).collect();
    assert_eq!(close, [51272.85, 51126.0]);
}

/// A fractional strike goes as a number and the expiry in its own century
/// (the symbol-format doc's `VEDL25APR24292.5CE`; no web case).
#[tokio::test]
async fn a_fractional_strike_and_its_expiry_are_sent_as_they_read() {
    let fake = Fake::start().await;
    fake.on(HISTORICAL, vec![Reply::Json(200, historical_answer())]);
    let b = fake.broker(master());

    b.get_history(
        &session(),
        &req("VEDL25APR24292.5CE", "NFO", "1m", jan(1), jan(1)),
    )
    .await
    .unwrap();

    let body = fake.seen()[0].json();
    assert_eq!(body["symbol"], "VEDL");
    assert_eq!(body["expiry_date"], "2024-04-25");
    assert_eq!(body["strike_price"], json!(292.5));
}

/// Web test_pocketful_data.py::test_nfo_future_history_sends_no_strike_and_the_documented_interval
/// (today, 2025 being long past, is outside the range: no quote is asked).
#[tokio::test]
async fn nfo_future_history_sends_no_strike_and_the_documented_interval() {
    for (interval, pocketful) in [("1h", "60minute"), ("D", "1day")] {
        let fake = Fake::start().await;
        fake.on(HISTORICAL, vec![Reply::Json(200, historical_answer())]);
        let b = fake.broker(master());

        b.get_history(
            &session(),
            &req("BANKNIFTY29JAN25FUT", "NFO", interval, jan(1), jan(1)),
        )
        .await
        .unwrap();

        assert_eq!(paths(&fake), [HISTORICAL], "{}", interval);
        assert_eq!(
            fake.seen()[0].json(),
            json!({
                "symbol": "BANKNIFTY",
                "product_type": "futures",
                "expiry_date": "2025-01-29",
                "interval": pocketful,
                "from_date": "2025-01-01T00:00:00",
                "to_date": "2025-01-01T23:59:59",
            }),
            "{}",
            interval
        );
    }
}

/// Web test_pocketful_data.py::test_nfo_history_falls_back_to_chart_candles
#[tokio::test]
async fn nfo_history_falls_back_to_chart_candles() {
    for historical in [Reply::Json(200, error_answer()), empty_candles()] {
        let fake = Fake::start().await;
        fake.on(HISTORICAL, vec![historical.clone()]);
        fake.on(CHARTS, vec![Reply::Json(200, chart_answer())]);
        let b = fake.broker(master());

        let bars = b
            .get_history(
                &session(),
                &req("BANKNIFTY29JAN2551000CE", "NFO", "1m", jan(1), jan(1)),
            )
            .await
            .unwrap();

        assert_eq!(paths(&fake), [HISTORICAL, CHARTS], "{:?}", historical);
        let query = fake.to(CHARTS)[0].query_pairs();
        assert!(
            query.contains(&("exchange".into(), "NFO".into())),
            "{:?}",
            query
        );
        assert!(
            query.contains(&("token".into(), "35002".into())),
            "{:?}",
            query
        );
        let close: Vec<f64> = bars.iter().map(|c| c.close).collect();
        assert_eq!(close, [137.85, 137.7]);
        let oi: Vec<i64> = bars.iter().map(|c| c.oi).collect();
        assert_eq!(oi, [0, 0]);
    }
}

/// Web test_pocketful_data.py::test_an_ended_session_does_not_fall_back
#[tokio::test]
async fn an_ended_session_does_not_fall_back() {
    let fake = Fake::start().await;
    fake.on(HISTORICAL, vec![unauthorised()]);
    fake.on(CHARTS, vec![Reply::Json(200, chart_answer())]);
    let b = fake.broker(master());

    let e = b
        .get_history(
            &session(),
            &req("BANKNIFTY29JAN2551000CE", "NFO", "1m", jan(1), jan(1)),
        )
        .await
        .unwrap_err();

    assert!(matches!(e, AppError::Auth(_)), "{:?}", e);
    assert_eq!(paths(&fake), [HISTORICAL]);
}

/// Web test_pocketful_data.py::test_bfo_history_uses_chart_candles
#[tokio::test]
async fn bfo_history_uses_chart_candles() {
    let fake = Fake::start().await;
    fake.on(CHARTS, vec![Reply::Json(200, chart_answer())]);
    let b = fake.broker(master());

    b.get_history(
        &session(),
        &req("SENSEX24DEC2580000CE", "BFO", "1m", jan(1), jan(1)),
    )
    .await
    .unwrap();

    assert_eq!(paths(&fake), [CHARTS]);
    let query = fake.seen()[0].query_pairs();
    assert!(
        query.contains(&("exchange".into(), "BFO".into())),
        "{:?}",
        query
    );
}
