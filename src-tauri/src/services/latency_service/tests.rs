//! The web's test/test_latency_groups.py, its report and retention cases,
//! on a `logs.db` of its own: these figures are totals, and one stray
//! record changes them.

use super::*;
use crate::db::sqlite::logs::LogsDb;
use rusqlite::Connection;

struct Db {
    _dir: tempfile::TempDir,
    db: LogsDb,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let db = LogsDb::new(&dir.path().join("logs.db")).unwrap();
    Db { _dir: dir, db }
}

fn now() -> DateTime<Utc> {
    chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, 14, 4, 30, 0).unwrap()
}

/// One record kind (web `_record`): `rtt` defaults to 80% of the total.
struct Rec<'a> {
    tag: Option<&'a str>,
    total: f64,
    count: usize,
    broker: &'a str,
    failed: bool,
    age_days: i64,
    rtt: Option<f64>,
}

fn rec(tag: &str, total: f64, count: usize) -> Rec<'_> {
    Rec {
        tag: Some(tag),
        total,
        count,
        broker: "zerodha",
        failed: false,
        age_days: 0,
        rtt: None,
    }
}

fn add(c: &Connection, r: Rec) {
    let rtt = r.rtt.unwrap_or(r.total * 0.8);
    for _ in 0..r.count {
        c.execute(
            "INSERT INTO order_latency (timestamp, order_id, broker, symbol, order_type, rtt_ms,
                 validation_latency_ms, response_latency_ms, overhead_ms, total_latency_ms, status)
             VALUES (?1, '1', ?2, 'SBIN', ?3, ?4, 0, 0, ?5, ?6, ?7)",
            rusqlite::params![
                store::ts(now() - Duration::days(r.age_days)),
                r.broker,
                r.tag,
                rtt,
                r.total - rtt,
                r.total,
                if r.failed { "FAILED" } else { "SUCCESS" }
            ],
        )
        .unwrap();
    }
}

fn stats(c: &Connection, group: &'static str) -> Value {
    report(c, group, now()).unwrap()
}

fn counts(v: &Value) -> Vec<(String, i64)> {
    v["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["key"].as_str().unwrap().to_string(),
                g["count"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn close(v: &Value, want: f64) -> bool {
    (v.as_f64().unwrap() - want).abs() < 1e-9
}

#[test]
fn numpy_percentiles() {
    let v = [1.0, 2.0, 3.0, 4.0];
    assert_eq!(percentile(&v, 50.0), 2.5);
    assert_eq!(percentile(&v, 100.0), 4.0);
}

/// Web test_data_calls_are_not_counted_as_orders: the report of #2421,
/// history calls hid the order latency.
#[test]
fn data_calls_are_not_counted_as_orders() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("PLACE", 60.0, 3));
    add(&c, rec("HISTORY", 1200.0, 200));
    let orders = stats(&c, "orders");
    assert_eq!(orders["group"], "orders");
    assert_eq!(orders["rated"], true);
    assert_eq!(orders["total_orders"], 3);
    assert!(close(&orders["avg_total"], 60.0));
    assert!(close(&orders["p99_total"], 60.0));
    assert!(close(&orders["sla_150ms"], 100.0));
    assert_eq!(
        orders["distribution"],
        json!({"excellent": 3, "good": 0, "acceptable": 0, "slow": 0})
    );
    let data = stats(&c, "market_data");
    assert_eq!(
        (data["total_orders"].clone(), data["rated"].clone()),
        (json!(200), json!(false))
    );
    assert!(close(&data["avg_total"], 1200.0));
    let everything = stats(&c, "all");
    assert_eq!(everything["total_orders"], 203);
    assert!(close(
        &everything["avg_total"],
        (3.0 * 60.0 + 200.0 * 1200.0) / 203.0
    ));
    assert!(orders.get("broker_histograms").is_none());
}

/// Web test_an_install_with_no_orders_shows_none: what the page showed as
/// "2,607 orders tracked".
#[test]
fn an_install_with_no_orders_shows_none() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("HISTORY", 40.0, 25));
    add(&c, rec("ORDERBOOK", 5.0, 4));
    let orders = stats(&c, "orders");
    assert_eq!(orders["total_orders"], 0);
    assert_eq!(orders["avg_total"], 0.0);
    assert_eq!(orders["endpoints"], json!([]));
    assert_eq!(orders["broker_stats"], json!({}));
    let want: Vec<(String, i64)> = [
        ("orders", 0),
        ("account", 4),
        ("market_data", 25),
        ("options", 0),
        ("reference", 0),
        ("tools", 0),
        ("all", 29),
    ]
    .iter()
    .map(|(k, n)| (k.to_string(), *n))
    .collect();
    assert_eq!(counts(&orders), want);
}

/// Web test_each_endpoint_of_a_group_is_reported_on_its_own: history is
/// expected to be slow and is not averaged into the quotes.
#[test]
fn each_endpoint_of_a_group_is_reported_on_its_own() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("QUOTES", 20.0, 10));
    add(
        &c,
        Rec {
            failed: true,
            ..rec("HISTORY", 900.0, 5)
        },
    );
    add(&c, rec("HISTORY", 1100.0, 5));
    let v = stats(&c, "market_data");
    let rows = v["endpoints"].as_array().unwrap();
    let types: Vec<&str> = rows.iter().map(|r| r["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["QUOTES", "HISTORY"], "in their group's order");
    assert_eq!(
        (rows[0]["label"].as_str(), rows[1]["label"].as_str()),
        (Some("Quote"), Some("History"))
    );
    assert_eq!(rows[0]["total_orders"], 10);
    assert!(close(&rows[0]["avg_total"], 20.0) && close(&rows[0]["p99_total"], 20.0));
    assert_eq!(
        (
            rows[1]["total_orders"].clone(),
            rows[1]["failed_orders"].clone()
        ),
        (json!(10), json!(5))
    );
    assert!(close(&rows[1]["avg_total"], 1000.0) && close(&rows[1]["p50_total"], 1000.0));
}

/// Web test_an_average_across_endpoints_is_weighted_by_how_often_each_was_called.
#[test]
fn an_average_across_endpoints_is_weighted_by_how_often_each_was_called() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("PLACE", 100.0, 1));
    add(&c, rec("CANCEL", 10.0, 9));
    assert!(close(&stats(&c, "orders")["avg_total"], 19.0));
}

/// Web test_the_broker_table_and_the_split_of_time_are_for_the_group.
#[test]
fn the_broker_table_and_the_split_of_time_are_for_the_group() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(
        &c,
        Rec {
            rtt: Some(90.0),
            ..rec("PLACE", 100.0, 2)
        },
    );
    add(
        &c,
        Rec {
            broker: "dhan",
            rtt: Some(250.0),
            ..rec("PLACE", 300.0, 2)
        },
    );
    add(&c, rec("HISTORY", 2000.0, 50));
    let v = stats(&c, "orders");
    let brokers = v["broker_stats"].as_object().unwrap();
    assert_eq!(brokers.keys().collect::<Vec<_>>(), ["dhan", "zerodha"]);
    assert_eq!(brokers["zerodha"]["total_orders"], 2);
    assert!(close(&brokers["zerodha"]["avg_total"], 100.0));
    assert!(close(&brokers["zerodha"]["sla_150ms"], 100.0));
    assert!(close(&brokers["dhan"]["sla_150ms"], 0.0));
    assert!(close(&v["avg_rtt"], 170.0) && close(&v["avg_overhead"], 30.0));
}

/// Web test_the_speed_bands_count_every_record_of_the_group.
#[test]
fn the_speed_bands_count_every_record_of_the_group() {
    let d = db();
    let c = d.db.conn().unwrap();
    for (total, n) in [(100.0, 150), (200.0, 30), (300.0, 15), (500.0, 5)] {
        add(&c, rec("PLACE", total, n));
    }
    add(&c, rec("HISTORY", 5000.0, 300));
    let v = stats(&c, "orders");
    assert_eq!(
        v["distribution"],
        json!({"excellent": 150, "good": 30, "acceptable": 15, "slow": 5})
    );
    assert!(
        close(&v["sla_100ms"], 0.0) && close(&v["sla_150ms"], 75.0) && close(&v["sla_200ms"], 75.0)
    );
}

/// Web test_rows_written_under_the_old_options_tags_are_options_orders, and
/// the desktop's own old options tool tags.
#[test]
fn rows_written_under_old_tags_are_read_as_what_they_are() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("OPTIONSORDER", 80.0, 2));
    add(&c, rec("OPTIONS", 120.0, 2));
    add(&c, rec("OPTIONSMULTIORDER", 200.0, 1));
    add(&c, rec("GREEKS", 30.0, 3));
    let v = stats(&c, "orders");
    assert_eq!(v["total_orders"], 5);
    let rows = v["endpoints"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        (rows[0]["type"].as_str(), rows[0]["total_orders"].as_i64()),
        (Some("OPTIONS"), Some(4))
    );
    assert!(close(&rows[0]["avg_total"], 100.0));
    assert_eq!(rows[0]["label"], "Options order");
    assert_eq!(
        (rows[1]["type"].as_str(), rows[1]["total_orders"].as_i64()),
        (Some("OPTIONS_MULTI"), Some(1))
    );
    let options = stats(&c, "options");
    assert_eq!(options["endpoints"][0]["type"], "OPTIONGREEKS");
    assert_eq!(options["total_orders"], 3);
}

/// Web test_a_record_in_no_group_is_other_and_other_is_listed_only_when_there_is_one.
#[test]
fn a_record_in_no_group_is_other_and_other_is_listed_only_when_there_is_one() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("PLACE", 50.0, 1));
    assert!(!counts(&stats(&c, "orders"))
        .iter()
        .any(|(k, _)| k == "other"));
    add(&c, rec("SOMETHING_NEW", 70.0, 2));
    add(
        &c,
        Rec {
            tag: None,
            ..rec("", 90.0, 1)
        },
    );
    let v = stats(&c, "orders");
    let n = counts(&v);
    assert!(
        n.contains(&("other".into(), 3)) && n.contains(&("all".into(), 4)),
        "{n:?}"
    );
    let other = stats(&c, "other");
    assert_eq!(other["total_orders"], 3);
    let labels: Vec<&str> = other["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["SOMETHING_NEW", "Unknown"]);
    assert_eq!(stats(&c, "all")["total_orders"], 4);
}

/// Web test_percentiles_cover_the_recent_window_and_counts_cover_everything.
#[test]
fn percentiles_cover_the_recent_window_and_counts_cover_everything() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(&c, rec("PLACE", 50.0, 10));
    add(
        &c,
        Rec {
            age_days: PERCENTILE_DAYS + 5,
            ..rec("PLACE", 9000.0, 1)
        },
    );
    let v = stats(&c, "orders");
    assert_eq!(v["total_orders"], 11);
    assert!(close(&v["p99_total"], 50.0));
    assert_eq!(v["percentile_window_days"], PERCENTILE_DAYS);
}

/// Web test_a_group_says_how_long_its_records_are_kept.
#[test]
fn a_group_says_how_long_its_records_are_kept() {
    let d = db();
    let c = d.db.conn().unwrap();
    assert_eq!(stats(&c, "orders")["retention_days"], Value::Null);
    assert_eq!(
        stats(&c, "market_data")["retention_days"],
        groups::RETENTION_DAYS
    );
    assert_eq!(stats(&c, "all")["retention_days"], groups::RETENTION_DAYS);
}

/// Web test_the_recent_list_of_a_group_is_not_pushed_out_by_other_calls.
#[test]
fn the_recent_list_of_a_group_is_not_pushed_out_by_other_calls() {
    let d = db();
    let c = d.db.conn().unwrap();
    add(
        &c,
        Rec {
            age_days: 1,
            ..rec("PLACE", 60.0, 2)
        },
    );
    add(&c, rec("HISTORY", 1000.0, 150));
    let tags = |group: &str, limit| -> Vec<String> {
        store::recent_latency(&c, &LatencyFilter::for_group(group), limit)
            .unwrap()
            .into_iter()
            .map(|l| l.order_type)
            .collect()
    };
    assert!(tags("all", Some(100)).iter().all(|t| t == "HISTORY"));
    assert_eq!(tags("orders", Some(100)), ["PLACE", "PLACE"]);
    assert_eq!(tags("all", None).len(), 152);
}

/// Web test_old_data_records_go_and_every_kind_of_order_stays.
#[test]
fn old_data_records_go_and_every_kind_of_order_stays() {
    let d = db();
    let c = d.db.conn().unwrap();
    let old = groups::RETENTION_DAYS + 1;
    add(
        &c,
        Rec {
            age_days: old,
            ..rec("HISTORY", 1000.0, 4)
        },
    );
    add(
        &c,
        Rec {
            age_days: old,
            ..rec("GTT_ORDERBOOK", 20.0, 1)
        },
    );
    add(&c, rec("HISTORY", 1000.0, 2));
    for tag in [
        "PLACE",
        "OPTIONS",
        "OPTIONS_MULTI",
        "OPTIONSORDER",
        "OPTIONSMULTIORDER",
        "GTT_PLACE",
    ] {
        add(
            &c,
            Rec {
                age_days: old,
                ..rec(tag, 80.0, 1)
            },
        );
    }
    let before = store::count(&c, "order_latency").unwrap();
    store::purge(&c, now()).unwrap();
    assert_eq!(before - store::count(&c, "order_latency").unwrap(), 5);
    let mut left: Vec<String> = store::recent_latency(&c, &LatencyFilter::All, None)
        .unwrap()
        .into_iter()
        .map(|l| l.order_type)
        .collect();
    left.sort();
    let mut want = vec![
        "HISTORY",
        "HISTORY",
        "PLACE",
        "OPTIONS",
        "OPTIONS_MULTI",
        "OPTIONSORDER",
        "OPTIONSMULTIORDER",
        "GTT_PLACE",
    ];
    want.sort();
    assert_eq!(left, want);
}

#[test]
fn an_empty_log_reports_zeros_with_every_key() {
    let d = db();
    let c = d.db.conn().unwrap();
    let v = stats(&c, "orders");
    for key in [
        "group",
        "groups",
        "label",
        "description",
        "rated",
        "retention_days",
        "percentile_window_days",
        "total_orders",
        "failed_orders",
        "success_rate",
        "avg_rtt",
        "avg_overhead",
        "avg_total",
        "p50_total",
        "p90_total",
        "p95_total",
        "p99_total",
        "sla_100ms",
        "sla_150ms",
        "sla_200ms",
        "distribution",
        "endpoints",
        "broker_stats",
    ] {
        assert!(v.get(key).is_some(), "{key}");
    }
    assert_eq!(v["label"], "Orders");
}
