//! Records OpenAlgo meant to save and did not (OBS-01, OBS-02): the order
//! log subscriber's shed and failed entries and the monitor's lost records
//! are on the Health page and raise an alert; the monitor saves what is
//! queued when OpenAlgo quits.

use crate::webui_support;

use openalgo_desktop_lib::db::sqlite::monitor::{self as store, TrafficRow};
use openalgo_desktop_lib::events::bus::BEST_EFFORT_CAP;
use openalgo_desktop_lib::events::{Event, Mode};
use openalgo_desktop_lib::services::monitor::Record;
use openalgo_desktop_lib::services::{core, health_service, monitor, record_loss};
use serde_json::json;
use std::time::Duration;
use webui_support::*;

fn cancelled(i: usize) -> Event {
    Event::OrderCancelled {
        meta: core::meta(
            Mode::Live,
            "cancelorder",
            json!({"orderid": i.to_string()}),
            &json!({"status": "success", "orderid": i.to_string()}),
        ),
        orderid: i.to_string(),
        status: "success".into(),
    }
}

/// OBS-01: entries the log subscriber's full queue shed are counted per
/// subscriber, shown on the Health page and raise an alert that says what
/// was lost. Nothing awaits between the publishes, so the subscriber's
/// worker (same runtime thread) has taken none: exactly 10 are shed.
#[tokio::test]
async fn shed_order_log_entries_show_in_health_and_raise_an_alert() {
    let (h, c, _) = signed_in();
    for i in 0..(BEST_EFFORT_CAP + 10) {
        h.ctx.bus.publish(cancelled(i));
    }
    let (s, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(s, 200, "{}", v);
    let r = &v["records"];
    assert_eq!(r["order_logs_dropped"], 10, "{}", r);
    assert_eq!(r["status"], "warn");
    let log = r["subscribers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["name"] == "log")
        .cloned()
        .unwrap();
    assert_eq!(log["dropped"], 10);
    assert_eq!(log["lane"], "best_effort");

    health_service::sample_once(&h.ctx).unwrap();
    let alerts = health_service::active_alerts(&h.ctx).unwrap();
    let a = alerts
        .iter()
        .find(|a| a.alert_type == record_loss::ALERT)
        .expect("a records-not-saved alert");
    assert!(a.message.contains("10 order log entries"), "{}", a.message);

    // Resolved by the trader, it stays resolved until something else is lost.
    health_service::settle_alert(&h.ctx, a.id, true).unwrap();
    health_service::sample_once(&h.ctx).unwrap();
    assert!(!health_service::active_alerts(&h.ctx)
        .unwrap()
        .iter()
        .any(|a| a.alert_type == record_loss::ALERT));

    // The system report carries the same counts.
    let (s, _, body) = h
        .send(with(get("/admin/api/system/report"), &c, None))
        .await;
    assert_eq!(s, 200);
    let report = String::from_utf8(body).unwrap();
    assert!(
        report.contains("**Order log entries dropped:** 10"),
        "{}",
        report
    );
}

/// OBS-01: an order log entry whose write fails is counted.
#[tokio::test]
async fn an_order_log_entry_that_cannot_be_written_is_counted() {
    let (h, c, _) = signed_in();
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute_batch("DROP TABLE order_logs")
        .unwrap();
    h.ctx.bus.publish(cancelled(1));
    for _ in 0..500 {
        if h.ctx.logs.failed_log_writes() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (_, v) = h.json(with(get("/health/api/current"), &c, None)).await;
    assert_eq!(v["records"]["order_logs_failed"], 1, "{}", v["records"]);
    assert_eq!(v["records"]["order_logs_dropped"], 0);
}

/// OBS-02: what the monitor holds when OpenAlgo quits is saved, not lost.
/// The writer has not run before shutdown (nothing awaited), so all 500
/// records are still queued when shutdown begins.
#[tokio::test]
async fn monitoring_records_queued_at_shutdown_are_saved() {
    let h = H::new();
    monitor::start(&h.ctx);
    for i in 0..500 {
        h.ctx.monitor.send(Record::Traffic(TrafficRow {
            timestamp: store::ts(h.ctx.now()),
            client_ip: "127.0.0.1".into(),
            method: "GET".into(),
            path: format!("/quit/{}", i),
            status_code: 200,
            duration_ms: 1.0,
            host: None,
            error: None,
        }));
    }
    h.ctx.shutdown().await;
    let saved = store::count(&h.ctx.logs.conn().unwrap(), "traffic_logs").unwrap();
    assert_eq!(saved, 500);
    assert_eq!(h.ctx.monitor.lost(), 0);
    assert_eq!(h.ctx.monitor.dropped(), 0);
    // After shutdown the queue is closed: a late record is counted.
    h.ctx.monitor.send(Record::InvalidKey {
        ip: "10.0.0.9".into(),
    });
    assert_eq!(h.ctx.monitor.dropped(), 1);
}
