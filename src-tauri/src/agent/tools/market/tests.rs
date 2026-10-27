//! The market toolkit against the web's tests (`test_agent_tool_arguments.py`)
//! and against the mock broker through the in-process `/api/v1`.

use super::candles::*;
use super::normalise::*;
use super::*;
use crate::agent::tools::testkit::{body, map, tag, Kit};
use crate::brokers::types::{Candle, Quote};
use chrono::NaiveDate;
use serde_json::json;

const ACCEPTED: [&str; 7] = ["1m", "5m", "15m", "1h", "D", "W", "M"];

fn accepted(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The web's `listed` fixture: a fixed set of listed pairs.
fn listed(symbol: &str, exchange: &str) -> bool {
    [
        ("INFY", "NSE"),
        ("SBIN", "NSE"),
        ("RELIANCE", "NSE"),
        ("NIFTY", "NSE_INDEX"),
        ("SENSEX", "BSE_INDEX"),
    ]
    .contains(&(symbol, exchange))
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 9).unwrap_or_default()
}

fn refused(r: Result<impl std::fmt::Debug, ToolOutcome>, needle: &str) {
    match r {
        Err(ToolOutcome::Retry(text)) => assert!(text.contains(needle), "{}", text),
        other => panic!("expected a refusal naming {:?}, got {:?}", needle, other),
    }
}

fn v(s: &str) -> Value {
    json!(s)
}

#[test]
fn every_exchange_spelling_reads_as_the_code() {
    for spelling in ["nse index", "NSE-INDEX", " nse_index ", "Nse  Index"] {
        assert_eq!(
            clean_exchange(Some(&v(spelling))),
            "NSE_INDEX",
            "{}",
            spelling
        );
    }
}

#[test]
fn accepted_date_spellings() {
    for spelling in [
        "2026-10-09",
        "2026-10-09T00:00:00",
        "2026-10-09T15:30:00+05:30",
        "2026-10-09 09:15",
        "09-10-2026",
        "9-10-2026",
        "2026/10/09",
        "09-Oct-2026",
        "09-OCT-2026",
    ] {
        assert_eq!(
            normalise_date("start_date", Some(&v(spelling)), today()).ok(),
            Some("2026-10-09".into()),
            "{}",
            spelling
        );
    }
}

#[test]
fn today_and_yesterday_are_ist() {
    assert_eq!(
        normalise_date("end_date", Some(&v("today")), today()).ok(),
        Some("2026-10-09".into())
    );
    assert_eq!(
        normalise_date("end_date", Some(&v(" Yesterday ")), today()).ok(),
        Some("2026-10-08".into())
    );
}

#[test]
fn anything_else_is_still_refused_as_a_date() {
    for spelling in [
        "next friday",
        "31-02-2026",
        "2026-13-01",
        "09-Foo-2026",
        "10/09/2026",
        "",
    ] {
        refused(
            normalise_date("start_date", Some(&v(spelling)), today()),
            "Use YYYY-MM-DD",
        );
    }
    assert_eq!(
        normalise_date("start_date", Some(&v("tomorrow")), today()),
        Err(ToolOutcome::Retry(
            "The 'start_date' argument is invalid: 'tomorrow' is not a date Use YYYY-MM-DD, for example 2026-01-15."
                .into()
        ))
    );
}

#[test]
fn history_ranges_fill_what_was_left_out() {
    let lookback = lookback_range("D", DEFAULT_HISTORY_BARS, today());
    let both_out = history_range("D", None, None, DEFAULT_HISTORY_BARS, today());
    assert_eq!(
        both_out.clone().ok(),
        Some((lookback.0.clone(), "2026-10-09".into()))
    );
    // 100 daily bars: int(100 * 1.6) + 5 = 165 calendar days.
    assert_eq!(lookback.0, "2026-04-27");
    let empty = history_range(
        "D",
        Some(&v("")),
        Some(&v("")),
        DEFAULT_HISTORY_BARS,
        today(),
    );
    assert_eq!(empty, both_out);
    let end = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap_or_default();
    assert_eq!(
        history_range(
            "D",
            None,
            Some(&v("2026-03-31")),
            DEFAULT_HISTORY_BARS,
            today()
        )
        .ok(),
        Some((
            lookback_range("D", DEFAULT_HISTORY_BARS, end).0,
            "2026-03-31".into()
        ))
    );
    assert_eq!(
        history_range(
            "D",
            Some(&v("2026-01-01")),
            None,
            DEFAULT_HISTORY_BARS,
            today()
        )
        .ok(),
        Some(("2026-01-01".into(), "2026-10-09".into()))
    );
    assert_eq!(
        history_range(
            "5m",
            Some(&v("2026-01-01")),
            Some(&v("2026-01-31")),
            DEFAULT_HISTORY_BARS,
            today()
        )
        .ok(),
        Some(("2026-01-01".into(), "2026-01-31".into()))
    );
    refused(
        history_range(
            "D",
            Some(&v("2026-02-01")),
            Some(&v("2026-01-01")),
            DEFAULT_HISTORY_BARS,
            today(),
        ),
        "start_date",
    );
}

#[test]
fn the_lookback_follows_the_web_arithmetic() {
    assert_eq!(calendar_days_for_bars(100, "D"), 165);
    assert_eq!(calendar_days_for_bars(100, "5m"), 7);
    assert_eq!(calendar_days_for_bars(100, "W"), 805);
    assert_eq!(calendar_days_for_bars(100, "M"), 3205);
    assert_eq!(calendar_days_for_bars(100, "unknown"), 7);
    // The ceiling: 4000 calendar days.
    assert_eq!(
        lookback_range("M", 1000, today()).0,
        (today() - chrono::Duration::days(4000))
            .format("%Y-%m-%d")
            .to_string()
    );
}

#[test]
fn interval_aliases_are_translated_with_a_notice() {
    let list = accepted(&ACCEPTED);
    for (raw, want) in [
        ("1d", "D"),
        ("1D", "D"),
        ("day", "D"),
        ("Daily", "D"),
        ("1day", "D"),
        ("1w", "W"),
        ("week", "W"),
        ("weekly", "W"),
        ("1mo", "M"),
        ("month", "M"),
        ("Monthly", "M"),
        ("5min", "5m"),
        ("5mins", "5m"),
        ("15minute", "15m"),
        ("15 minutes", "15m"),
        ("1hr", "1h"),
        ("1hour", "1h"),
        ("1 hours", "1h"),
    ] {
        let (got, notice) = normalise_interval(Some(&v(raw)), "api", Some(&list))
            .unwrap_or_else(|e| panic!("{}: {:?}", raw, e));
        assert_eq!(got, want, "{}", raw);
        assert!(
            notice.unwrap_or_default().contains(&format!("'{}'", raw)),
            "{}",
            raw
        );
    }
}

#[test]
fn accepted_intervals_pass_and_case_is_corrected() {
    let list = accepted(&ACCEPTED);
    for value in ACCEPTED {
        assert_eq!(
            normalise_interval(Some(&v(value)), "api", Some(&list)).ok(),
            Some((value.to_string(), None))
        );
    }
    let (five, notice) = normalise_interval(Some(&v("5M")), "api", Some(&list))
        .unwrap_or_else(|e| panic!("{:?}", e));
    assert_eq!(five, "5m");
    assert_eq!(
        notice.as_deref(),
        Some("The interval was read as '5m' rather than '5M'. Interval names are case sensitive: 'm' is minutes and 'M' is months.")
    );
}

#[test]
fn sixty_minutes_becomes_an_hour_only_where_the_broker_lacks_60m() {
    let list = accepted(&ACCEPTED);
    for raw in ["60m", "60min"] {
        let (got, notice) = normalise_interval(Some(&v(raw)), "api", Some(&list))
            .unwrap_or_else(|e| panic!("{:?}", e));
        assert_eq!(got, "1h");
        assert!(notice.is_some());
    }
    let with_60 = accepted(&["1m", "60m", "D"]);
    assert_eq!(
        normalise_interval(Some(&v("60m")), "api", Some(&with_60)).ok(),
        Some(("60m".into(), None))
    );
    refused(
        normalise_interval(Some(&v("60m")), "api", Some(&accepted(&["1m", "5m", "D"]))),
        "Use one of",
    );
    refused(
        normalise_interval(Some(&v("2hours")), "api", Some(&list)),
        "Use one of",
    );
    for raw in ["fortnight", "1y", "quarterly", "5 candles"] {
        refused(
            normalise_interval(Some(&v(raw)), "api", Some(&list)),
            "'interval' argument",
        );
    }
}

#[test]
fn aliases_apply_to_the_local_store_and_an_unknown_list() {
    assert_eq!(
        normalise_interval(Some(&v("1d")), "db", None)
            .map(|r| r.0)
            .ok(),
        Some("D".into())
    );
    assert_eq!(
        normalise_interval(Some(&v("5min")), "api", None)
            .map(|r| r.0)
            .ok(),
        Some("5m".into())
    );
    for source in ["db", "api"] {
        assert_eq!(
            normalise_interval(Some(&v("3D")), source, None).ok(),
            Some(("3D".into(), None))
        );
    }
    refused(normalise_interval(Some(&v("")), "api", None), "it is empty");
}

#[test]
fn the_daily_interval_follows_the_broker() {
    assert_eq!(daily_interval(Some(&accepted(&["1m", "D"]))), "D");
    assert_eq!(daily_interval(Some(&accepted(&["1m", "1d"]))), "1d");
    assert_eq!(daily_interval(Some(&[])), "D");
    assert_eq!(daily_interval(None), "D");
}

#[test]
fn a_prefixed_symbol_needs_no_exchange_and_wins() {
    assert_eq!(
        normalise_pair(&listed, Some(&v("NSE:INFY")), Some(&v(""))).ok(),
        Some(("INFY".into(), "NSE".into(), vec![]))
    );
    assert_eq!(
        normalise_pair(&listed, Some(&v("nse index:NIFTY")), None).ok(),
        Some(("NIFTY".into(), "NSE_INDEX".into(), vec![]))
    );
    let (s, e, notices) = normalise_pair(&listed, Some(&v("NSE:INFY")), Some(&v("BSE")))
        .unwrap_or_else(|r| panic!("{:?}", r));
    assert_eq!((s.as_str(), e.as_str()), ("INFY", "NSE"));
    assert_eq!(
        notices,
        vec!["The symbol was written as 'NSE:INFY', so NSE was used rather than the exchange BSE passed beside it."]
    );
    refused(
        normalise_pair(&listed, Some(&v("FOO:BAR")), Some(&v(""))),
        "'exchange' argument",
    );
    refused(
        normalise_pair(&listed, Some(&v("INFY")), Some(&v(""))),
        "'exchange' argument",
    );
    refused(
        normalise_pair(&listed, Some(&v("")), Some(&v("NSE"))),
        "'symbol' argument",
    );
    refused(
        normalise_pair(&listed, Some(&v("INFY")), Some(&v("LSE"))),
        "'LSE' is not an OpenAlgo exchange code",
    );
}

#[test]
fn an_index_on_its_cash_exchange_still_moves() {
    let (s, e, notices) = normalise_pair(&listed, Some(&v("NIFTY")), Some(&v("nse")))
        .unwrap_or_else(|r| panic!("{:?}", r));
    assert_eq!((s.as_str(), e.as_str()), ("NIFTY", "NSE_INDEX"));
    assert_eq!(
        notices,
        vec!["NIFTY is not listed on NSE, but it is listed on NSE_INDEX, so NSE_INDEX was used. Index values are quoted on the index exchanges and cannot be traded there; the tradable instrument is the index future or option."]
    );
    // An unknown symbol is left for the service to report.
    assert_eq!(
        resolve_exchange(&listed, "NOSUCH", "NSE"),
        ("NSE".into(), None)
    );
}

#[test]
fn symbol_lists_take_every_documented_shape() {
    let pairs = |raw: Value| {
        symbol_pairs(&listed, Some(&raw), "symbols", 50, false)
            .unwrap_or_else(|r| panic!("{:?}", r))
    };
    let (three, notices) = pairs(v("NSE:INFY, NSE:SBIN; NSE_INDEX:NIFTY"));
    assert_eq!(
        three,
        vec![
            ("INFY".to_string(), "NSE".to_string()),
            ("SBIN".into(), "NSE".into()),
            ("NIFTY".into(), "NSE_INDEX".into())
        ]
    );
    assert!(notices.is_empty());
    assert_eq!(
        pairs(v(r#"[{"symbol":"INFY","exchange":"NSE"}]"#)).0,
        vec![("INFY".to_string(), "NSE".to_string())]
    );
    assert_eq!(
        pairs(json!([{"symbol": "NSE:SBIN"}])).0,
        vec![("SBIN".to_string(), "NSE".to_string())]
    );
    assert_eq!(
        pairs(json!({"Symbol": "INFY", "EXCHANGE": "NSE"})).0,
        vec![("INFY".to_string(), "NSE".to_string())]
    );
    let (deduped, notices) = pairs(json!(["NSE:INFY", "NSE:INFY"]));
    assert_eq!(deduped.len(), 1);
    assert_eq!(
        notices,
        vec!["1 duplicate entries were requested once each rather than repeatedly."]
    );
    refused(
        symbol_pairs(&listed, Some(&v("INFY, SBIN")), "symbols", 50, false),
        "'symbols' argument",
    );
    refused(
        symbol_pairs(&listed, Some(&json!([])), "symbols", 50, false),
        "it is empty or is not a list",
    );
    refused(
        symbol_pairs(&listed, Some(&json!([5])), "symbols", 50, false),
        "entry 1 is int, not an object",
    );
    refused(
        symbol_pairs(
            &listed,
            Some(&json!([{"symbol": "INFY"}])),
            "symbols",
            50,
            false,
        ),
        "entry 1 is missing 'symbol' or 'exchange'",
    );
    let many: Vec<Value> = (0..3).map(|_| v("NSE:INFY")).collect();
    refused(
        symbol_pairs(
            &listed,
            Some(&Value::Array(many.clone())),
            "symbols",
            2,
            false,
        ),
        "it carries 3 entries, more than the 2 allowed in one call",
    );
    let (kept, notices) = symbol_pairs(&listed, Some(&Value::Array(many)), "symbols", 2, true)
        .unwrap_or_else(|r| panic!("{:?}", r));
    assert_eq!(kept.len(), 1);
    assert_eq!(
        notices[0],
        "1 of 3 entries were dropped, because this call carries at most 2."
    );
}

#[test]
fn weekly_and_monthly_roll_up_from_daily_candles() {
    let daily_only = accepted(&["1m", "5m", "15m", "1h", "D"]);
    let (fetched, notice, rollup) = plan_interval(Some(&v("W")), "api", Some(&daily_only))
        .unwrap_or_else(|e| panic!("{:?}", e));
    assert_eq!((fetched.as_str(), rollup.as_deref()), ("D", Some("W")));
    assert!(notice
        .unwrap_or_default()
        .contains("weekly candles were built from its daily ones"));
    assert_eq!(
        plan_interval(Some(&v("weekly")), "api", Some(&daily_only))
            .ok()
            .and_then(|p| p.2),
        Some("W".into())
    );
    assert_eq!(
        plan_interval(Some(&v("monthly")), "api", Some(&daily_only))
            .ok()
            .and_then(|p| p.2),
        Some("M".into())
    );
    assert_eq!(
        plan_interval(Some(&v("W")), "api", Some(&accepted(&["D", "W", "M"]))).ok(),
        Some(("W".into(), None, None))
    );
    assert_eq!(
        plan_interval(Some(&v("5m")), "api", Some(&daily_only)).ok(),
        Some(("5m".into(), None, None))
    );
    assert!(plan_interval(Some(&v("W")), "api", Some(&accepted(&["1m", "5m"]))).is_err());
}

fn day(d: u32, o: f64, h: f64, l: f64, c: f64, vol: i64) -> Map<String, Value> {
    let stamp = IST
        .with_ymd_and_hms(2026, 10, d, 0, 0, 0)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
        .unwrap_or_default();
    map(json!({"timestamp": stamp, "open": o, "high": h, "low": l, "close": c, "volume": vol}))
}

use chrono::TimeZone;

#[test]
fn daily_candles_roll_up_into_iso_weeks() {
    let rows = vec![
        day(5, 100.0, 105.0, 99.0, 104.0, 10),
        day(7, 104.0, 110.0, 101.0, 108.0, 20),
        day(9, 108.0, 109.0, 95.0, 97.0, 30),
        day(12, 97.0, 99.0, 96.0, 98.0, 5),
    ];
    let columns = candle_columns(&rows[0]);
    let weeks = roll_up_candles(&rows, &columns, "W");
    assert_eq!(weeks.len(), 2);
    assert_eq!(weeks[0]["open"], 100.0);
    assert_eq!(weeks[0]["close"], 97.0);
    assert_eq!(weeks[0]["high"], 110.0);
    assert_eq!(weeks[0]["low"], 95.0);
    assert_eq!(weeks[0]["volume"], 60.0);
    assert_eq!(weeks[0]["timestamp"], rows[0]["timestamp"]);
    assert_eq!(weeks[1]["volume"], 5.0);
    let months = roll_up_candles(&rows, &columns, "M");
    assert_eq!(months.len(), 1);
}

#[test]
fn the_summary_covers_every_row_and_omits_what_no_row_had() {
    let rows = vec![
        map(
            json!({"timestamp": 1_791_000_000, "open": 10, "high": 12, "low": 9, "close": 11, "volume": 100}),
        ),
        map(
            json!({"timestamp": 1_791_086_400, "open": "11", "high": 15, "low": 8, "close": 14, "volume": 50}),
        ),
    ];
    let columns = candle_columns(&rows[0]);
    let s = summarise_candles(&rows, &columns);
    assert_eq!(
        Value::Object(s),
        json!({
            "first_timestamp": "2026-10-03T09:30:00+05:30",
            "last_timestamp": "2026-10-04T09:30:00+05:30",
            "first_open": 10.0,
            "last_close": 14.0,
            "highest_high": 15.0,
            "highest_high_timestamp": "2026-10-04T09:30:00+05:30",
            "lowest_low": 8.0,
            "lowest_low_timestamp": "2026-10-04T09:30:00+05:30",
            "total_volume": 150.0,
            "change": 4.0,
            "change_percent": 40.0
        })
    );
    let no_volume = vec![map(json!({"timestamp": 1_791_000_000, "close": 5}))];
    let s = summarise_candles(&no_volume, &candle_columns(&no_volume[0]));
    assert!(s.get("total_volume").is_none() && s.get("first_open").is_none());
    assert!(summarise_candles(&[], &Columns::default()).is_empty());
}

#[test]
fn chart_bars_drop_unusable_rows_and_sort() {
    let rows = vec![
        map(json!({"Timestamp": 1_791_086_400_000_i64, "Close": 2, "Volume": 7})),
        map(json!({"Timestamp": 1_791_000_000, "Open": 1, "High": 3, "Low": 0.5, "Close": 1.5})),
        map(json!({"Timestamp": "", "Close": 9})),
        map(json!({"Timestamp": 1_791_000_001, "Close": null})),
    ];
    let bars = chart_bars(&rows, &candle_columns(&rows[0]));
    assert_eq!(
        bars.into_iter().map(Value::Object).collect::<Vec<_>>(),
        vec![
            json!({"time": 1_791_000_000, "open": 1.0, "high": 3.0, "low": 0.5, "close": 1.5}),
            json!({"time": 1_791_086_400, "open": 2.0, "high": 2.0, "low": 2.0, "close": 2.0, "volume": 7.0}),
        ]
    );
    assert_eq!(epoch_seconds(&v("2026-10-09 09:15")), Some(1_791_517_500));
    assert_eq!(ist_label(&json!(1_791_517_500)), "2026-10-09 09:15");
    assert_eq!(epoch_seconds(&json!(true)), None);
}

#[test]
fn fit_to_budget_keeps_the_most_rows_that_fit() {
    let rows: Vec<Value> = (0..500)
        .map(|i| json!({"i": i, "pad": "x".repeat(60)}))
        .collect();
    let payload = fit_to_budget(
        |limit| json!({"rows": rows[..limit.min(rows.len())].to_vec(), "kept": limit}),
        rows.len(),
    );
    let kept = payload["kept"].as_u64().unwrap_or_default() as usize;
    assert!(rendered_length(&payload) <= JSON_BUDGET);
    let one_more = json!({"rows": rows[..kept + 1].to_vec(), "kept": kept + 1});
    assert!(rendered_length(&one_more) > JSON_BUDGET, "kept {}", kept);
}

// ---------------------------------------------------------------- the tools

fn quote(symbol: &str, exchange: &str, ltp: f64) -> Quote {
    Quote {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp,
        open: ltp - 10.0,
        high: ltp + 5.0,
        low: ltp - 12.0,
        close: ltp - 8.0,
        volume: 1000,
        ..Default::default()
    }
}

#[tokio::test]
async fn get_quote_reads_the_broker_and_moves_an_index() {
    let kit = Kit::new();
    kit.mock.set_quote(quote("INFY", "NSE", 1520.5));
    let out = kit
        .call(
            &GROUP,
            "get_quote",
            json!({"symbol": "infy", "exchange": "nse"}),
        )
        .await;
    assert!(out.is_ok(), "{:?}", out);
    assert_eq!(
        tag(&out),
        "<tool_result tool=\"get_quote\" symbol=\"INFY\" exchange=\"NSE\">"
    );
    let b = body(&out);
    assert_eq!(
        (b["symbol"].as_str(), b["exchange"].as_str()),
        (Some("INFY"), Some("NSE"))
    );
    assert_eq!(b["quote"]["ltp"], 1520.5);
    assert!(b.get("notices").is_none());

    kit.mock.set_quote(quote("NIFTY", "NSE_INDEX", 25000.0));
    let out = kit
        .call(
            &GROUP,
            "get_quote",
            json!({"symbol": "NIFTY", "exchange": "NSE"}),
        )
        .await;
    let b = body(&out);
    assert_eq!(b["exchange"], "NSE_INDEX");
    assert!(b["notices"][0]
        .as_str()
        .unwrap_or_default()
        .starts_with("NIFTY is not listed on NSE"));

    // A broker failure is a correction naming the service, not a crash.
    let out = kit
        .call(
            &GROUP,
            "get_quote",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await;
    assert!(
        matches!(&out, ToolOutcome::Retry(t) if t.starts_with("quotes_service.get_quotes failed (HTTP ")),
        "{:?}",
        out
    );
}

#[tokio::test]
async fn get_quotes_takes_text_and_keeps_failures_in_their_rows() {
    let kit = Kit::new();
    kit.mock.set_quote(quote("INFY", "NSE", 1520.5));
    let out = kit
        .call(
            &GROUP,
            "get_quotes",
            json!({"symbols": "NSE:INFY, NSE:SBIN"}),
        )
        .await;
    assert_eq!(tag(&out), "<tool_result tool=\"get_quotes\" count=\"2\">");
    let b = body(&out);
    assert_eq!(b["requested"], 2);
    assert_eq!(b["results_returned"], 2);
    assert_eq!(b["results"].as_array().map(Vec::len), Some(2));
    // Every documented shape passes the schema.
    for shape in [
        json!(["NSE:INFY", "NSE:SBIN"]),
        json!("NSE:INFY, NSE:SBIN"),
        json!([{"symbol": "INFY", "exchange": "NSE"}, "NSE:SBIN"]),
    ] {
        let out = kit
            .call(&GROUP, "get_quotes", json!({"symbols": shape}))
            .await;
        assert!(out.is_ok(), "{:?}", out);
    }
    let fifty_one: Vec<Value> = (0..51).map(|_| v("NSE:INFY")).collect();
    let out = kit
        .call(&GROUP, "get_quotes", json!({"symbols": fifty_one}))
        .await;
    assert!(out.text().contains("more than the 50 allowed"), "{:?}", out);
}

#[tokio::test]
async fn get_depth_answers_with_the_book() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_depth",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await;
    assert!(out.is_ok(), "{:?}", out);
    let b = body(&out);
    assert_eq!(b["symbol"], "SBIN");
    assert!(b["depth"].is_object(), "{}", b);
}

fn candles(n: i64) -> Vec<Candle> {
    (0..n)
        .map(|i| Candle {
            timestamp: 1_759_000_000 + i * 86_400,
            open: 100.0 + i as f64,
            high: 105.0 + i as f64,
            low: 95.0 + i as f64,
            close: 101.0 + i as f64,
            volume: 1000,
            oi: 0,
        })
        .collect()
}

#[tokio::test]
async fn get_history_with_only_a_symbol_asks_for_daily_candles_to_today() {
    let kit = Kit::new();
    let out = kit
        .call(
            &GROUP,
            "get_history",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    assert!(out.is_ok(), "{:?}", out);
    let b = body(&out);
    assert_eq!(b["rows_total"], 0);
    assert_eq!(b["interval"], "D");
    assert_eq!(b["end_date"], "2026-10-09");
    assert_eq!(
        b["start_date"],
        lookback_range("D", DEFAULT_HISTORY_BARS, today()).0
    );
    assert_eq!(
        b["note"],
        "The range returned no candles. Check the dates against the trading calendar and confirm the interval is one this broker supports."
    );
    let asked = kit.mock.calls().into_iter().find_map(|c| match c {
        crate::brokers::mock::MockCall::History(r) => Some(r),
        _ => None,
    });
    assert!(asked
        .is_some_and(|r| format!("{:?}", r).contains("\"D\"") || format!("{:?}", r).contains("D")));
    // The defaults live in the tool, as agno's schema carries none.
    let spec = crate::agent::tools::specs::web("get_history").unwrap_or_else(|| panic!("spec"));
    assert_eq!(spec.required(), vec!["symbol", "exchange"]);
}

#[tokio::test]
async fn a_long_history_keeps_the_newest_candles_within_the_cap() {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(candles(400)));
    let out = kit
        .call(
            &GROUP,
            "get_history",
            json!({"symbol": "INFY", "exchange": "NSE", "start_date": "2025-01-01", "end_date": "2026-10-09"}),
        )
        .await;
    let b = body(&out);
    assert_eq!(b["rows_total"], 400);
    let returned = b["rows_returned"].as_u64().unwrap_or_default();
    assert!(
        returned > 0 && returned <= MAX_HISTORY_ROWS as u64,
        "{}",
        returned
    );
    assert_eq!(b["rows_omitted"].as_u64(), Some(400 - returned));
    assert!(b["note"].as_str().unwrap_or_default().starts_with(&format!(
        "{} older candles of 400 were omitted",
        400 - returned
    )));
    // The summary covers all 400, not only the tail.
    assert_eq!(b["summary"]["first_open"], 100.0);
    assert_eq!(b["summary"]["last_close"], 500.0);
    assert!(out.text().chars().count() <= MAX_RESULT_CHARS + 200);
    let last = &b["candles"]
        .as_array()
        .and_then(|c| c.last().cloned())
        .unwrap_or_default();
    assert_eq!(last["close"], 500.0);
}

#[tokio::test]
async fn get_history_answers_weekly_from_daily() {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(candles(14)));
    let out = kit
        .call(
            &GROUP,
            "get_history",
            json!({"symbol": "INFY", "exchange": "NSE", "interval": "W"}),
        )
        .await;
    let b = body(&out);
    assert_eq!(b["interval"], "W");
    assert!(b["rows_total"].as_u64().unwrap_or_default() >= 2);
    assert!(
        out.text().contains("built from its daily ones"),
        "{}",
        out.text()
    );
    let refusal = kit
        .call(
            &GROUP,
            "get_history",
            json!({"symbol": "INFY", "exchange": "NSE", "interval": "fortnight"}),
        )
        .await;
    assert!(
        refusal.text().contains("'interval' argument"),
        "{:?}",
        refusal
    );
    let bad_source = kit
        .call(
            &GROUP,
            "get_history",
            json!({"symbol": "INFY", "exchange": "NSE", "source": "cache"}),
        )
        .await;
    assert_eq!(
        bad_source,
        ToolOutcome::Retry("The 'source' argument is invalid: 'cache' is not a data source Use 'api' for live broker history, or 'db' for candles already downloaded into the local Historify store.".into())
    );
}

#[tokio::test]
async fn get_intervals_groups_the_broker_list() {
    let kit = Kit::new();
    let out = kit.call(&GROUP, "get_intervals", json!({})).await;
    let b = body(&out);
    assert_eq!(tag(&out), "<tool_result tool=\"get_intervals\">");
    let accepted: Vec<&str> = b["accepted"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(accepted.contains(&"D") && accepted.contains(&"5m"), "{}", b);
    assert!(b["intervals"].is_object());
}

#[tokio::test]
async fn the_market_toolkit_matches_the_web() {
    let kit = Kit::new();
    let ctx = kit.ctx(crate::agent::contract::tool::Surface::Chat);
    crate::agent::tools::spec_contract::assert_toolkit(&GROUP, &ctx);
}

#[test]
fn a_candle_frame_has_no_gaps_and_one_row_per_timestamp() {
    let rows = vec![
        map(
            json!({"timestamp": 300, "open": 1, "high": 2, "low": 0.5, "close": 1.5, "volume": null}),
        ),
        map(
            json!({"timestamp": 100, "open": 1, "high": 2, "low": 0.5, "close": 1.0, "volume": null}),
        ),
        map(
            json!({"timestamp": 200, "open": 1, "high": null, "low": 0.5, "close": 1.2, "volume": null}),
        ),
        map(
            json!({"timestamp": 100, "open": 1, "high": 2, "low": 0.5, "close": 1.1, "volume": null}),
        ),
        map(
            json!({"timestamp": 300, "open": 1, "high": null, "low": 0.5, "close": 9.9, "volume": null}),
        ),
        map(
            json!({"timestamp": "", "open": 1, "high": 2, "low": 0.5, "close": 7.0, "volume": null}),
        ),
    ];
    let frame = candle_frame(&rows, &candle_columns(&rows[0]));
    // volume was never supplied: the column goes, the rows stay.
    assert_eq!(frame.column("volume"), None);
    assert_eq!(frame.time, vec![100, 300]);
    assert_eq!(frame.column("close"), Some(&[1.1, 1.5][..]));
    assert_eq!(frame.len(), 2);
    assert!(candle_frame(&[], &Columns::default()).is_empty());
    let no_close = vec![map(json!({"timestamp": 1, "open": 1}))];
    assert!(candle_frame(&no_close, &candle_columns(&no_close[0])).is_empty());
}
