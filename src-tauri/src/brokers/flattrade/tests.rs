//! Flattrade (PiConnect) cases the web pins in its own tests, run through
//! the generic Noren adapter with Flattrade's config.

use super::config;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::mapping::map_trade;
use crate::brokers::families::noren::NorenConfig;
use crate::brokers::types::Trade;
use crate::brokers::{shoonya, tradesmart, zebu};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Trade book: one row per fill (web test/test_flattrade_issue_2464.py)
// ---------------------------------------------------------------------------

/// The reporter's SELL 120 of SENSEX19NOV2682300PE (web #2464).
const ORDER: &str = "26111700329676";
const SYMBOL: &str = "SENSEX19NOV2682300PE";

/// A PiConnect TradeBook row as the API sends it, every number a string
/// (web `fill`): `qty` and `avgprc` are the order's, `flqty` and `flprc`
/// the fill's own, left out when `None`.
fn fill(
    order: &str,
    trantype: &str,
    qty: i64,
    flqty: Option<&str>,
    flprc: Option<&str>,
    avgprc: &str,
) -> Value {
    let mut row = json!({
        "stat": "Ok",
        "norenordno": order,
        "exch": "BFO",
        "tsym": SYMBOL,
        "prd": "NRML",
        "trantype": trantype,
        "qty": qty.to_string(),
        "fillshares": qty.to_string(),
        "avgprc": avgprc,
        "norentm": "13:09:10 17-11-2026",
    });
    if let Some(q) = flqty {
        row["flqty"] = json!(q);
    }
    if let Some(p) = flprc {
        row["flprc"] = json!(p);
    }
    row
}

/// The SELL 120 order, filled 60 + 60. Each row carries qty 120 and the
/// order's running average at that fill (316.05, then 316.02), as the
/// reporter's trade book showed (web `SPLIT_SELL`).
fn split_sell() -> Vec<Value> {
    vec![
        fill(ORDER, "SELL", 120, Some("60"), Some("316.05"), "316.05"),
        fill(ORDER, "SELL", 120, Some("60"), Some("315.99"), "316.02"),
    ]
}

fn book(cfg: &'static NorenConfig, rows: &[Value]) -> Vec<Trade> {
    let symbols = SymbolResolver::new();
    rows.iter().map(|t| map_trade(cfg, t, &symbols)).collect()
}

fn trades(rows: &[Value]) -> Vec<Trade> {
    book(config(), rows)
}

/// `pytest.approx`: equal within a relative 1e-6.
fn approx(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-6 * expected.abs()
}

fn sum(trades: &[Trade], side: Option<&str>, of: impl Fn(&Trade) -> f64) -> f64 {
    trades
        .iter()
        .filter(|t| side.is_none_or(|s| t.side == s))
        .map(of)
        .sum()
}

/// Web `test_flattrade_issue_2464.py::test_a_part_filled_order_is_counted_once`.
#[test]
fn a_part_filled_order_is_counted_once() {
    let t = trades(&split_sell());
    let quantities: Vec<i32> = t.iter().map(|x| x.quantity).collect();
    assert_eq!(quantities, [60, 60]);
    assert_eq!(sum(&t, None, |x| f64::from(x.quantity)), 120.0);
    let prices: Vec<f64> = t.iter().map(|x| x.average_price).collect();
    assert_eq!(prices, [316.05, 315.99]);
    // Value of the whole order: 120 at the order's average of 316.02.
    let value = sum(&t, None, |x| x.trade_value);
    assert!(approx(value, 120.0 * 316.02), "{value}");
    assert!(t.iter().all(|x| x.order_id == ORDER));
}

/// Web `test_flattrade_issue_2464.py::test_the_reporters_strike_balances_on_a_flat_position`:
/// every SENSEX19NOV2682300PE fill of the reporter's day, 140 bought and
/// 140 sold, realised 579.40.
#[test]
fn the_reporters_strike_balances_on_a_flat_position() {
    let one = |order: &str, side: &str, qty: i64, price: &str| {
        let q = qty.to_string();
        fill(order, side, qty, Some(q.as_str()), Some(price), price)
    };
    let mut day = vec![
        one("26111700089790", "BUY", 20, "365.00"),
        one("26111700096590", "SELL", 20, "375.15"),
        one("26111700105305", "BUY", 20, "376.40"),
        one("26111700122313", "BUY", 20, "317.90"),
        one("26111700157141", "BUY", 20, "322.45"),
        one("26111700167056", "BUY", 40, "290.90"),
        one("26111700183899", "BUY", 20, "278.75"),
    ];
    day.extend(split_sell());
    let t = trades(&day);
    let bought = sum(&t, Some("BUY"), |x| f64::from(x.quantity));
    let sold = sum(&t, Some("SELL"), |x| f64::from(x.quantity));
    assert_eq!((bought, sold), (140.0, 140.0));
    let buy_value = sum(&t, Some("BUY"), |x| x.trade_value);
    let sell_value = sum(&t, Some("SELL"), |x| x.trade_value);
    assert!(
        approx(sell_value - buy_value, 579.40),
        "{}",
        sell_value - buy_value
    );
}

/// Web `test_flattrade_issue_2464.py::test_a_single_fill_order_is_unchanged`.
#[test]
fn a_single_fill_order_is_unchanged() {
    let t = trades(&[fill("1", "BUY", 20, Some("20"), Some("562.30"), "562.30")]);
    assert_eq!(
        t,
        [Trade {
            order_id: "1".into(),
            trade_id: "1".into(),
            symbol: SYMBOL.into(),
            exchange: "BFO".into(),
            product: "NRML".into(),
            side: "BUY".into(),
            quantity: 20,
            average_price: 562.3,
            trade_value: 11246.0,
            timestamp: "13:09:10 17-11-2026".into(),
            order_tag: None,
        }]
    );
}

/// Web `test_flattrade_issue_2464.py::test_a_row_without_fill_fields_falls_back_to_the_order_fields`.
#[test]
fn a_row_without_fill_fields_falls_back_to_the_order_fields() {
    let t = trades(&[fill("1", "BUY", 20, None, None, "562.30")]);
    assert_eq!((t[0].quantity, t[0].average_price), (20, 562.3));
}

/// Web `test_flattrade_issue_2464.py::test_an_unreadable_number_maps_to_zero_instead_of_failing_the_book`.
#[test]
fn an_unreadable_number_maps_to_zero_instead_of_failing_the_book() {
    let t = trades(&[fill("1", "BUY", 20, Some(""), Some(""), "562.30")]);
    assert_eq!(
        (t[0].quantity, t[0].average_price, t[0].trade_value),
        (0, 0.0, 0.0)
    );
}

/// The web fixed Flattrade only (#2480): its Shoonya, Zebu and TradeSmart
/// plugins still read the order's `qty` and `avgprc` from the same rows,
/// and the shared Noren mapping keeps that for them.
#[test]
fn the_other_noren_members_keep_reading_the_order_fields() {
    for cfg in [shoonya::config(), zebu::config(), tradesmart::config()] {
        let t = book(cfg, &split_sell());
        let quantities: Vec<i32> = t.iter().map(|x| x.quantity).collect();
        assert_eq!(quantities, [120, 120], "{}", cfg.id);
        let prices: Vec<f64> = t.iter().map(|x| x.average_price).collect();
        assert_eq!(prices, [316.05, 316.02], "{}", cfg.id);
    }
}

// ---------------------------------------------------------------------------
// History: each session opens at the pre-open discovered price, from the
// market calendar (web test/test_flattrade_preopen_history.py), through
// Flattrade's adapter, the history service and the app's own calendar
// (Muhurat 8 Nov 2026 at 14:20, a regular 9 Nov at 09:15).
// ---------------------------------------------------------------------------

mod pre_open_history {
    use super::*;
    use crate::brokers::common::symbols::tests::row;
    use crate::brokers::families::noren::{NorenBroker, NorenEndpoints};
    use crate::brokers::{Broker, BrokerRegistry};
    use crate::services::market_data_service::history;
    use crate::state::testing::TestCtx;
    use crate::state::BrokerSession;
    use axum::routing::post;
    use axum::{Json, Router};
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Asia::Kolkata;
    use std::sync::Arc;

    /// A TPSeries bar as PiConnect sends it (web `_bar`).
    fn bar(time: &str, o: f64, h: f64, l: f64, c: f64, v: i64) -> Value {
        json!({"time": time, "into": o.to_string(), "inth": h.to_string(),
               "intl": l.to_string(), "intc": c.to_string(), "intv": v.to_string(), "oi": "0"})
    }

    /// A local PiConnect answering every chart request with `rows`, as the
    /// web's test patches `get_api_response`; stopped when dropped.
    struct PiConnect {
        base: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for PiConnect {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn piconnect(rows: Vec<Value>) -> PiConnect {
        let answer = move || {
            let rows = rows.clone();
            async move { Json(Value::Array(rows)) }
        };
        let app = Router::new()
            .route("/PiConnectAPI/TPSeries", post(answer.clone()))
            .route("/PiConnectAPI/EODChartData", post(answer));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        PiConnect { base, task }
    }

    /// The app signed in to Flattrade, its master holding SBIN and NIFTY,
    /// TPSeries answering `rows`.
    async fn signed_in(rows: Vec<Value>) -> (TestCtx, PiConnect) {
        let fake = piconnect(rows).await;
        let symbols = SymbolResolver::new();
        let endpoints = NorenEndpoints::rebased(config(), &fake.base, "ws://127.0.0.1:9");
        let flattrade = NorenBroker::with_endpoints(config(), symbols.clone(), endpoints);
        let t = crate::state::testing::build(
            BrokerRegistry::with_symbols(symbols, vec![Arc::new(flattrade) as Arc<dyn Broker>]),
            chrono::Utc::now(),
        );
        t.ctx.load_symbol_cache(vec![
            row("SBIN", "SBIN-EQ", "NSE", "3045"),
            row("NIFTY", "Nifty 50", "NSE_INDEX", "26000"),
        ]);
        t.ctx.set_broker_session(Some(BrokerSession {
            broker_id: "flattrade".into(),
            auth_token: crate::security::Secret::new("<USER_ID>:::token"),
            feed_token: None,
            user_id: "<USER_ID>".into(),
            user_name: None,
            authenticated_at: t.ctx.now(),
        }));
        (t, fake)
    }

    type Row = (String, f64, f64, f64, f64, i64);

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
    }

    /// `/api/v1/history` for SBIN (NIFTY on `NSE_INDEX`) from `from` to
    /// `to`, read back as the web's `_history` does.
    async fn read(t: &TestCtx, exchange: &str, interval: &str, from: u32, to: u32) -> Vec<Row> {
        let symbol = if exchange == "NSE_INDEX" {
            "NIFTY"
        } else {
            "SBIN"
        };
        let r = history(
            &t.ctx,
            symbol,
            exchange,
            interval,
            day(from),
            day(to),
            "api",
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let ts = c["timestamp"].as_i64().unwrap();
                let t = Kolkata.timestamp_opt(ts, 0).unwrap();
                let n = |k: &str| c[k].as_f64().unwrap();
                let v = c["volume"].as_i64().unwrap();
                let t = t.format("%d %H:%M:%S").to_string();
                (t, n("open"), n("high"), n("low"), n("close"), v)
            })
            .collect()
    }

    async fn history_of(rows: Vec<Value>, exchange: &str, interval: &str) -> Vec<Row> {
        let (t, _fake) = signed_in(rows).await;
        read(&t, exchange, interval, 8, 9).await
    }

    fn at(t: &str, o: f64, h: f64, l: f64, c: f64, v: i64) -> Row {
        (t.to_string(), o, h, l, c, v)
    }

    /// TPSeries answers newest first; SBIN's real 8 Nov 2026 bars (web
    /// `MUHURAT_1M`).
    fn muhurat_1m() -> Vec<Value> {
        vec![
            bar(
                "08-11-2026 14:21:00",
                1022.95,
                1023.0,
                1020.0,
                1020.15,
                60619,
            ),
            bar(
                "08-11-2026 14:20:00",
                1023.7,
                1025.0,
                1020.05,
                1023.0,
                124262,
            ),
            bar("08-11-2026 14:19:00", 1023.2, 1023.2, 1023.2, 1023.2, 0),
            bar("08-11-2026 14:14:05", 1023.2, 1023.2, 1023.2, 1023.2, 1039),
            bar("08-11-2026 14:06:00", 1018.5, 1018.5, 1018.5, 1018.5, 2125),
            bar(
                "08-11-2026 14:05:00",
                1024.95,
                1018.5,
                1018.5,
                1018.5,
                15721,
            ),
        ]
    }

    /// 09:15 and 09:14 bars of the regular 9 Nov session (web
    /// `test_a_day_the_calendar_does_not_cover_keeps_every_bar`).
    fn regular_open(date: &str) -> Vec<Value> {
        vec![
            bar(&format!("{date} 09:15:00"), 101.0, 102.0, 100.0, 101.0, 10),
            bar(&format!("{date} 09:14:00"), 100.0, 100.0, 100.0, 100.0, 0),
        ]
    }

    /// Web `test_flattrade_preopen_history.py::test_muhurat_opens_at_the_discovered_price_without_pre_open_bars`.
    #[tokio::test]
    async fn muhurat_opens_at_the_discovered_price_without_pre_open_bars() {
        assert_eq!(
            history_of(muhurat_1m(), "NSE", "1m").await,
            [
                at("08 14:20:00", 1023.2, 1025.0, 1020.05, 1023.0, 124262),
                at("08 14:21:00", 1022.95, 1023.0, 1020.0, 1020.15, 60619),
            ]
        );
    }

    /// Web `test_flattrade_preopen_history.py::test_regular_day_opens_at_the_09_14_price`.
    #[tokio::test]
    async fn regular_day_opens_at_the_09_14_price() {
        let bars = vec![
            bar("09-11-2026 09:16:00", 1029.0, 1030.0, 1028.0, 1029.5, 10),
            bar("09-11-2026 09:15:00", 1027.6, 1029.0, 1027.0, 1028.5, 10),
            bar("09-11-2026 09:14:00", 1030.0, 1030.0, 1030.0, 1030.0, 0),
        ];
        assert_eq!(
            history_of(bars, "NSE", "1m").await,
            [
                at("09 09:15:00", 1030.0, 1030.0, 1027.0, 1028.5, 10),
                at("09 09:16:00", 1029.0, 1030.0, 1028.0, 1029.5, 10),
            ]
        );
    }

    /// Web `test_flattrade_preopen_history.py::test_coarser_interval_uses_the_last_pre_open_close`.
    #[tokio::test]
    async fn coarser_interval_uses_the_last_pre_open_close() {
        let bars = vec![
            bar(
                "08-11-2026 14:20:00",
                1023.7,
                1025.0,
                1020.0,
                1022.6,
                375072,
            ),
            bar("08-11-2026 14:10:00", 1018.5, 1023.2, 1018.5, 1023.2, 5353),
            bar(
                "08-11-2026 14:05:00",
                1024.95,
                1018.5,
                1018.5,
                1018.5,
                26911,
            ),
        ];
        assert_eq!(
            history_of(bars, "NSE", "5m").await,
            [at("08 14:20:00", 1023.2, 1025.0, 1020.0, 1022.6, 375072)]
        );
    }

    /// Web `test_flattrade_preopen_history.py::test_a_bar_running_past_the_open_is_moved_to_it_not_left_out`.
    #[tokio::test]
    async fn a_bar_running_past_the_open_is_moved_to_it_not_left_out() {
        let bars = vec![
            bar("08-11-2026 14:30:00", 1015.3, 1021.0, 1015.3, 1020.4, 500),
            bar("08-11-2026 14:15:00", 1023.2, 1025.0, 1015.0, 1016.0, 900),
        ];
        assert_eq!(
            history_of(bars, "NSE", "15m").await,
            [
                at("08 14:20:00", 1023.2, 1025.0, 1015.0, 1016.0, 900),
                at("08 14:30:00", 1015.3, 1021.0, 1015.3, 1020.4, 500),
            ]
        );
    }

    /// Web `test_flattrade_preopen_history.py::test_index_follows_its_exchange_session`.
    #[tokio::test]
    async fn index_follows_its_exchange_session() {
        let bars = vec![
            bar("09-11-2026 09:15:00", 25000.0, 25010.0, 24990.0, 25005.0, 0),
            bar("09-11-2026 09:14:00", 24980.0, 24980.0, 24980.0, 24980.0, 0),
        ];
        assert_eq!(
            history_of(bars, "NSE_INDEX", "1m").await,
            [at("09 09:15:00", 24980.0, 25010.0, 24980.0, 25005.0, 0)]
        );
    }

    /// Web `test_flattrade_preopen_history.py::test_a_day_the_calendar_does_not_cover_keeps_every_bar`:
    /// the calendar has no NSE session on Saturday 14 Nov 2026, so nothing
    /// is assumed about it.
    #[tokio::test]
    async fn a_day_the_calendar_does_not_cover_keeps_every_bar() {
        let (t, _fake) = signed_in(regular_open("14-11-2026")).await;
        let times: Vec<String> = read(&t, "NSE", "1m", 14, 14)
            .await
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(times, ["14 09:14:00", "14 09:15:00"]);
    }

    /// Web `test_flattrade_preopen_history.py::test_a_calendar_read_failure_keeps_every_bar`.
    #[tokio::test]
    async fn a_calendar_read_failure_keeps_every_bar() {
        let (t, _fake) = signed_in(regular_open("09-11-2026")).await;
        t.ctx
            .sqlite
            .conn()
            .unwrap()
            .execute_batch("DROP TABLE market_timings")
            .unwrap();
        let times: Vec<String> = read(&t, "NSE", "1m", 9, 9)
            .await
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(times, ["09 09:14:00", "09 09:15:00"]);
    }

    /// Web `test_flattrade_preopen_history.py::test_daily_candles_are_untouched`.
    #[tokio::test]
    async fn daily_candles_are_untouched() {
        let eod = json!({"time": "08-NOV-2026", "ssboe": "1794076200", "into": "1023.2",
            "inth": "1025", "intl": "1015.1", "intc": "1016", "intv": "1950983"});
        let (t, _fake) = signed_in(vec![eod]).await;
        let opens: Vec<f64> = read(&t, "NSE", "D", 8, 8)
            .await
            .into_iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(opens, [1023.2]);
    }
}
