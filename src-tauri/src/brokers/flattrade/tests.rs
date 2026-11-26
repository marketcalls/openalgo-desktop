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
// Order status of a part-filled order: the order's average, not one fill's.
// The web's orderstatus takes the first trade book row with the order's id,
// which since #2480 is one Flattrade fill: a web defect not copied (lane110
// review, `account_service::traded_average_price`).
// ---------------------------------------------------------------------------

mod order_status {
    use super::*;
    use crate::agent::tools::testkit::Kit;
    use crate::brokers::families::noren::mapping::map_order;
    use crate::brokers::types::Order;
    use crate::services::account_service;

    /// The SELL 120 as PiConnect's OrderBook reports it once complete: 120
    /// filled, at the order's average of 316.02.
    fn complete_sell(cfg: &'static NorenConfig) -> Order {
        let row = json!({
            "stat": "Ok",
            "norenordno": ORDER,
            "exch": "BFO",
            "tsym": SYMBOL,
            "trantype": "S",
            "qty": "120",
            "fillshares": "120",
            "prc": "0.00",
            "avgprc": "316.02",
            "prctyp": "MKT",
            "status": "COMPLETE",
            "prd": "M",
            "norentm": "13:09:10 17-11-2026",
        });
        map_order(cfg, &row, &SymbolResolver::new())
    }

    /// `average_price` of `/api/v1/orderstatus` for the SELL 120, with the
    /// order book and the trade `rows` both read through `cfg`.
    async fn average_price(kit: &Kit, cfg: &'static NorenConfig, rows: &[Value]) -> Value {
        *kit.mock.order_book.lock() = Some(Ok(vec![complete_sell(cfg)]));
        *kit.mock.trade_book.lock() = Some(Ok(book(cfg, rows)));
        let reply = account_service::order_status(kit.app(), &json!({"orderid": ORDER})).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.body["data"]["order_status"], "complete");
        reply.body["data"]["average_price"].clone()
    }

    /// The split SELL with its last fill listed first.
    fn last_fill_first() -> Vec<Value> {
        let mut rows = split_sell();
        rows.reverse();
        rows
    }

    /// Flattrade's rows are the fills, 60 at 316.05 and 60 at 315.99: the
    /// order status answers their average, 316.02, whichever fill the book
    /// lists first. Reading the first row answered 316.05 or 315.99.
    #[tokio::test]
    async fn a_part_filled_order_reports_the_average_of_its_fills() {
        let kit = Kit::new();
        assert_eq!(
            average_price(&kit, config(), &split_sell()).await,
            json!(316.02)
        );
        assert_eq!(
            average_price(&kit, config(), &last_fill_first()).await,
            json!(316.02)
        );
    }

    /// One fill of the whole order answers that fill's price.
    #[tokio::test]
    async fn a_single_fill_reports_its_own_price() {
        let kit = Kit::new();
        let one = [fill(
            ORDER,
            "SELL",
            120,
            Some("120"),
            Some("316.10"),
            "316.10",
        )];
        assert_eq!(average_price(&kit, config(), &one).await, json!(316.1));
    }

    /// Shoonya, Zebu and TradeSmart repeat the order's quantity and running
    /// average on every row, so their rows add up to 240 of a 120 order and
    /// the order status keeps the web's first row, as before.
    #[tokio::test]
    async fn the_other_noren_members_keep_the_first_rows_price() {
        let kit = Kit::new();
        for cfg in [shoonya::config(), zebu::config(), tradesmart::config()] {
            assert_eq!(
                average_price(&kit, cfg, &split_sell()).await,
                json!(316.05),
                "{}",
                cfg.id
            );
            assert_eq!(
                average_price(&kit, cfg, &last_fill_first()).await,
                json!(316.02),
                "{}",
                cfg.id
            );
        }
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

// ---------------------------------------------------------------------------
// Order updates on the one PiConnect socket (web 73599f37c, #2507)
// ---------------------------------------------------------------------------

/// Web #2507 keeps its order-stream change to Flattrade, whose PiConnect
/// allows one socket per login (issue #1806): its feed is the session's
/// only stream (the order catch-up after each reconnect, no connect after
/// the daily boundary), and the other Noren members keep their behaviour.
#[test]
fn only_flattrade_streams_its_orders_on_its_one_socket() {
    use crate::brokers::Broker;
    let s = SymbolResolver::new();
    assert!(super::broker(s.clone()).single_session_stream());
    for b in [
        shoonya::broker(s.clone()),
        zebu::broker(s.clone()),
        tradesmart::broker(s),
    ] {
        assert!(!b.single_session_stream(), "{}", b.id());
    }
}

mod order_stream {
    //! Flattrade's order stream on its one PiConnect socket, through the
    //! runtime a session runs (web 73599f37c, marketcalls/openalgo#2507,
    //! issue #2386), against a local PiConnect.

    use crate::brokers::common::symbols::SymbolResolver;
    use crate::brokers::families::noren::{NorenBroker, NorenEndpoints};
    use crate::brokers::flattrade::config;
    use crate::brokers::{Broker, BrokerRegistry};
    use crate::events::{bus::Lane, bus::Subscriber, Event, Topic};
    use crate::security::Secret;
    use crate::state::{testing, BrokerSession};
    use crate::websocket::FeedStatus;
    use axum::routing::post;
    use axum::{Json, Router};
    use chrono::{DateTime, TimeZone, Utc};
    use chrono_tz::Asia::Kolkata;
    use futures_util::{SinkExt, StreamExt};
    use parking_lot::Mutex;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    fn ist(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(2026, 11, d, h, m, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    /// An OrderBook row as PiConnect sends it: a limit buy of 10 RELIANCE.
    fn row(id: &str, status: &str, filled: &str) -> Value {
        json!({"stat": "Ok", "norenordno": id, "exch": "NSE", "tsym": "RELIANCE-EQ",
               "token": "2885", "trantype": "B", "qty": "10", "fillshares": filled,
               "prc": "1480.00", "prctyp": "LMT", "prd": "I", "status": status,
               "avgprc": "1480.00", "norentm": "10:20:00 18-11-2026", "ret": "DAY"})
    }

    /// A local PiConnect: the order book answers `book` (its reads
    /// counted), the socket acknowledges every login (counted) and holds
    /// the connection. Stopped when dropped.
    struct PiConnect {
        rest: String,
        ws: String,
        book: Arc<Mutex<Value>>,
        reads: Arc<AtomicUsize>,
        logins: Arc<AtomicUsize>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl Drop for PiConnect {
        fn drop(&mut self) {
            for t in &self.tasks {
                t.abort();
            }
        }
    }

    async fn piconnect(book: Value) -> PiConnect {
        let book = Arc::new(Mutex::new(book));
        let reads = Arc::new(AtomicUsize::new(0));
        let (answer, count) = (book.clone(), reads.clone());
        let app = Router::new().route(
            "/PiConnectAPI/OrderBook",
            post(move || {
                let (answer, count) = (answer.clone(), count.clone());
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let body = answer.lock().clone();
                    Json(body)
                }
            }),
        );
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rest = format!("http://{}", http.local_addr().unwrap());
        let rest_task = tokio::spawn(async move {
            let _ = axum::serve(http, app).await;
        });
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = format!("ws://{}", socket.local_addr().unwrap());
        let logins = Arc::new(AtomicUsize::new(0));
        let seen = logins.clone();
        let ws_task = tokio::spawn(async move {
            let mut held = tokio::task::JoinSet::new();
            while let Ok((tcp, _)) = socket.accept().await {
                let seen = seen.clone();
                held.spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    while let Some(Ok(m)) = ws.next().await {
                        if let Message::Text(t) = m {
                            if t.contains(r#""t":"a""#) {
                                seen.fetch_add(1, Ordering::SeqCst);
                                let ak = Message::Text(r#"{"t":"ak","s":"OK"}"#.into());
                                let _ = ws.send(ak).await;
                            }
                        }
                    }
                });
            }
        });
        PiConnect {
            rest,
            ws,
            book,
            reads,
            logins,
            tasks: vec![rest_task, ws_task],
        }
    }

    /// `order.update` events on the bus: order id and status.
    #[derive(Default)]
    struct Updates(Mutex<Vec<(String, String)>>);

    #[async_trait::async_trait]
    impl Subscriber for Updates {
        fn name(&self) -> &'static str {
            "flattrade-order-stream-test"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::OrderUpdate]
        }
        async fn handle(&self, event: Arc<Event>) {
            if let Event::OrderUpdate(u) = event.as_ref() {
                self.0
                    .lock()
                    .push((u.orderid.clone(), u.order_status.clone()));
            }
        }
    }

    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..1000 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {}", what);
    }

    /// Web `test_flattrade_issue_2386.py`'s catch-up and rollover, through
    /// a Flattrade session: the order book is read once the socket's login
    /// is accepted (that read only records it), once more after the socket
    /// reconnects (the order that filled meanwhile goes out as an order
    /// update), and once the daily boundary has passed the socket does not
    /// log in again and nothing is read, until the next sign-in.
    #[tokio::test]
    async fn the_session_catches_up_after_a_reconnect_and_stops_at_the_boundary() {
        let pi = piconnect(json!([row("26111800000001", "OPEN", "0")])).await;
        let endpoints = NorenEndpoints::rebased(config(), &pi.rest, &pi.ws);
        let symbols = SymbolResolver::new();
        let flattrade = NorenBroker::with_endpoints(config(), symbols.clone(), endpoints);
        let t = testing::build(
            BrokerRegistry::with_symbols(symbols, vec![Arc::new(flattrade) as Arc<dyn Broker>]),
            ist(18, 10, 0),
        );
        let ctx = &t.ctx;
        let updates = Arc::new(Updates::default());
        ctx.bus.subscribe(updates.clone(), Lane::Critical);
        let session = BrokerSession {
            broker_id: "flattrade".into(),
            auth_token: Secret::new("U1:::tok"),
            feed_token: None,
            user_id: "U1".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        };
        ctx.runtime.activate(ctx, &session).await;
        until("the order book read at the login", || {
            pi.reads.load(Ordering::SeqCst) == 1
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(updates.0.lock().is_empty(), "the first read only records");

        *pi.book.lock() = json!([row("26111800000001", "COMPLETE", "10")]);
        ctx.websocket.reconnect();
        until("the catch-up", || !updates.0.lock().is_empty()).await;
        assert_eq!(
            *updates.0.lock(),
            vec![("26111800000001".to_string(), "complete".to_string())]
        );
        assert_eq!(pi.reads.load(Ordering::SeqCst), 2);
        assert_eq!(pi.logins.load(Ordering::SeqCst), 2);

        t.clock.set(ist(19, 3, 1));
        ctx.websocket.reconnect();
        until("the feed to stand down", || {
            matches!(ctx.websocket.status(), FeedStatus::AuthFailed { .. })
        })
        .await;
        match ctx.websocket.status() {
            FeedStatus::AuthFailed { message, .. } => {
                assert!(message.contains("Log in to Flattrade again"), "{}", message)
            }
            other => panic!("{:?}", other),
        }
        assert_eq!(
            pi.logins.load(Ordering::SeqCst),
            2,
            "no login after the boundary"
        );
        assert_eq!(
            pi.reads.load(Ordering::SeqCst),
            2,
            "no read after the boundary"
        );
        ctx.runtime.teardown(ctx).await;
    }
}
