//! The Latency and Analyzer reports over a large log (DB-03, DB-01,
//! OBS-05): computed in SQL off the async workers, the export streamed,
//! and the analyzer's day statistics over every request of the day.

use crate::webui_support;

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use serde_json::json;
use std::net::SocketAddr;
use std::time::Duration;
use tower::ServiceExt;
use webui_support::*;

const ROWS: i64 = 200_000;

/// `ROWS` latency rows in one statement: three brokers, every tenth one
/// failed, all inside the last 30 days of the harness clock.
fn seed_latency(h: &H) {
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO order_latency (timestamp, order_id, broker, symbol, order_type, rtt_ms,
                 validation_latency_ms, response_latency_ms, overhead_ms, total_latency_ms, status)
             SELECT datetime('2026-10-05 04:00:00', '-' || (i % 2000000) || ' seconds'), i,
                 CASE i % 3 WHEN 0 THEN 'zerodha' WHEN 1 THEN 'dhan' ELSE 'fyers' END, 'SBIN',
                 'PLACE', (i % 397) * 1.0, 0, 0, i % 13, (i % 397) + (i % 13),
                 CASE WHEN i % 10 = 0 THEN 'FAILED' ELSE 'SUCCESS' END
             FROM n",
            [ROWS],
        )
        .unwrap();
}

/// DB-03, DB-01: four Latency page refreshes and an export over 200,000
/// rows never hold the runtime (`runtime_stall` under 200 ms). Before,
/// each report loaded every row and sorted it on the async worker (held
/// 7.5 s).
#[tokio::test]
async fn latency_reports_over_a_large_log_do_not_hold_the_runtime() {
    let (h, c, _) = signed_in();
    seed_latency(&h);
    // The router is built once, outside the measured span: the harness
    // builds the whole route table per request, which is the test's own
    // work on the runtime thread, not the server's.
    let app = openalgo_desktop_lib::server::app(h.ctx.clone());
    let call = |path: &str| {
        let mut r = with(get(path), &c, None);
        r.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        crate::with_host(&mut r, &h.ctx);
        let app = app.clone();
        async move {
            let resp = app.oneshot(r).await.unwrap();
            let status = resp.status();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            (status, body.to_vec())
        }
    };
    let stats = || call("/latency/api/stats");
    let ((a, b, d, e, (s, csv)), worst) = crate::runtime_stall(async {
        tokio::join!(stats(), stats(), stats(), stats(), call("/latency/export"))
    })
    .await;
    let json = |r: (StatusCode, Vec<u8>)| {
        (
            r.0,
            serde_json::from_slice::<serde_json::Value>(&r.1).unwrap(),
        )
    };
    let (a, b, d, e) = (json(a), json(b), json(d), json(e));
    for (s, v) in [&a, &b, &d, &e] {
        assert_eq!(*s, StatusCode::OK, "{}", v);
        assert_eq!(v["total_orders"], ROWS);
        assert_eq!(v["failed_orders"], ROWS / 10);
        assert!(v["avg_overhead"].as_f64().unwrap() > 0.0);
        assert_eq!(v["broker_stats"]["dhan"]["total_orders"], 66_667);
    }
    assert_eq!(s, StatusCode::OK);
    let csv = String::from_utf8(csv).unwrap();
    assert_eq!(csv.lines().count() as i64, ROWS + 1, "header and every row");
    assert!(csv.starts_with("Date & Time (IST),Broker,Order ID"));
    assert!(
        worst < Duration::from_millis(200),
        "the runtime was held for {:?}",
        worst
    );
}

/// OBS-05: the Analyzer page's day statistics count every request of the
/// last 24 hours, not only the newest 10,000 the page lists.
#[tokio::test]
async fn analyzer_day_statistics_count_every_request() {
    let (h, c, _) = signed_in();
    let newest = h.ctx.now() - chrono::Duration::hours(1);
    let at = |secs: i64| {
        (newest - chrono::Duration::seconds(secs))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    };
    {
        let mut conn = h.ctx.logs.conn().unwrap();
        let tx = conn.transaction().unwrap();
        // The oldest request in the window carries the only rate-limit error.
        tx.execute(
            "INSERT INTO analyzer_logs (api_type, request_data, response_data, created_at)
             VALUES ('placeorder', ?1, ?2, ?3)",
            rusqlite::params![
                json!({"symbol": "OLDEST", "strategy": "early"}).to_string(),
                json!({"status": "error", "message": "Rate limit exceeded"}).to_string(),
                at(20_000)
            ],
        )
        .unwrap();
        let mut st = tx
            .prepare(
                "INSERT INTO analyzer_logs (api_type, request_data, response_data, created_at)
                 VALUES ('placeorder', ?1, ?2, ?3)",
            )
            .unwrap();
        let req = json!({"symbol": "SBIN", "strategy": "s1"}).to_string();
        let ok = json!({"status": "success"}).to_string();
        for i in 0..10_000 {
            st.execute(rusqlite::params![req, ok, at(i)]).unwrap();
        }
        drop(st);
        tx.commit().unwrap();
    }
    let (s, v) = h.json(with(get("/analyzer/api/data"), &c, None)).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let stats = &v["data"]["stats"];
    assert_eq!(stats["total_requests"], 10_001, "{}", stats);
    assert_eq!(stats["issues"]["total"], 1);
    assert_eq!(stats["issues"]["by_type"]["rate_limit"], 1);
    assert_eq!(stats["symbols"], json!(["SBIN", "OLDEST"]));
    assert_eq!(stats["sources"], json!(["s1", "early"]));
}
