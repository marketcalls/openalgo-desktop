//! The Latency, Traffic, Logs, Health and Analyzer reports over a large log
//! (DB-03, DB-01, OBS-05, async-worker DB reads): computed in SQL off the
//! async workers, the exports read and formatted off them (the latency
//! export streamed), the polled statistics computed once for concurrent
//! refreshes, and the analyzer's day statistics over every request of the
//! day.

use crate::webui_support;

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use openalgo_desktop_lib::db::sqlite::monitor::{self as monitor_store, TrafficRow};
use openalgo_desktop_lib::services::monitor::Record;
use openalgo_desktop_lib::state::AppState;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
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

type App = tower_http::normalize_path::NormalizePath<axum::Router>;
type Reply = (StatusCode, Vec<u8>);

/// The whole router, built once before anything is measured: the harness
/// builds the route table per request, which is the test's own work on the
/// runtime thread, not the server's.
fn app(h: &H) -> App {
    openalgo_desktop_lib::server::app(h.ctx.clone())
}

/// GET `path` through `app` as the signed-in browser `c`, body collected.
async fn fetch(h: &H, app: &App, c: &str, path: &str) -> Reply {
    let mut r = with(get(path), c, None);
    r.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
    crate::with_host(&mut r, &h.ctx);
    let resp = app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec())
}

/// Four Latency page refreshes and an export, all at once.
async fn refreshes_and_export(h: &H, app: &App, c: &str) -> ([Reply; 4], Reply) {
    let stats = || fetch(h, app, c, "/latency/api/stats");
    let export = fetch(h, app, c, "/latency/export");
    let (a, b, d, e, x) = tokio::join!(stats(), stats(), stats(), stats(), export);
    ([a, b, d, e], x)
}

/// DB-03, DB-01: four Latency page refreshes and an export over 200,000
/// rows never hold the runtime. Before DB-03, each report loaded every row
/// and sorted it on the async worker (`runtime_stall` 7.5 s). Until the
/// export formatted its pages on the blocking pool, it spent 17 ms of the
/// async worker per page of 1,000 rows, 3.5 s in all (`runtime_busy`),
/// and a loaded CI runner stretched one page past 200 ms.
#[tokio::test]
async fn latency_reports_over_a_large_log_do_not_hold_the_runtime() {
    let (h, c, _) = signed_in();
    seed_latency(&h);
    let app = app(&h);
    let (((stats, (s, csv)), busy), worst) =
        crate::runtime_stall(crate::runtime_busy(refreshes_and_export(&h, &app, &c))).await;
    for (s, body) in stats {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(s, StatusCode::OK, "{}", v);
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
        "the runtime was held for {:?} at once",
        worst
    );
    assert!(
        busy < Duration::from_secs(1),
        "the reports kept the runtime busy for {:?} in all",
        busy
    );
}

/// Queue a traffic record and save the queue every 20 ms until `stop`, on
/// a thread of its own as the monitor's writer does; returns the slowest
/// save and how many records were sent. The harness runs no writer task,
/// so `drain_now` saves with the writer's own `save_batch`.
fn traffic_probe(ctx: Arc<AppState>, stop: Arc<AtomicBool>) -> JoinHandle<(Duration, i64)> {
    std::thread::spawn(move || {
        let (mut slowest, mut sent) = (Duration::ZERO, 0);
        while !stop.load(Ordering::Relaxed) {
            ctx.monitor.send(Record::Traffic(TrafficRow {
                timestamp: monitor_store::ts(ctx.now()),
                client_ip: "127.0.0.1".into(),
                method: "GET".into(),
                path: "/probe".into(),
                status_code: 200,
                duration_ms: 1.0,
                host: None,
                error: None,
            }));
            sent += 1;
            let t = Instant::now();
            ctx.monitor.drain_now(&ctx);
            slowest = slowest.max(t.elapsed());
            std::thread::sleep(Duration::from_millis(20));
        }
        (slowest, sent)
    })
}

/// Four Latency page refreshes and an export leave the logs database's
/// writers a connection: every traffic record saved meanwhile is written,
/// none waiting long. When each refresh computed its own report, the four
/// held all four of the logs pool's connections for the whole report
/// (seconds in a debug build): a save waited for one to finish, and failed
/// once that outlasted the pool's 10 s wait.
#[tokio::test]
async fn traffic_is_logged_while_latency_reports_run() {
    let (h, c, _) = signed_in();
    seed_latency(&h);
    let app = app(&h);
    let stop = Arc::new(AtomicBool::new(false));
    let probe = traffic_probe(h.ctx.clone(), stop.clone());
    let (stats, (s, _)) = refreshes_and_export(&h, &app, &c).await;
    stop.store(true, Ordering::Relaxed);
    let (slowest, sent) = probe.join().unwrap();
    assert!(stats.iter().all(|r| r.0 == StatusCode::OK));
    assert_eq!(s, StatusCode::OK);
    let saved: i64 = h
        .ctx
        .logs
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM traffic_logs WHERE path = '/probe'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(saved, sent, "every traffic record saved");
    assert_eq!(h.ctx.monitor.lost(), 0);
    assert!(
        slowest < Duration::from_secs(1),
        "a traffic save waited {:?} for the logs database",
        slowest
    );
}

/// The requests the traffic log keeps at most (`monitor::RETENTION`).
const TRAFFIC_ROWS: i64 = 100_000;

/// `TRAFFIC_ROWS` requests in one statement, a second apart.
fn seed_traffic(h: &H) {
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO traffic_logs (timestamp, client_ip, method, path, status_code,
                 duration_ms, host, error)
             SELECT datetime('2026-10-05 04:00:00', '-' || i || ' seconds'), '127.0.0.1', 'POST',
                 '/api/v1/placeorder', CASE WHEN i % 10 = 0 THEN 400 ELSE 200 END,
                 (i % 97) * 1.5, '127.0.0.1:5500', NULL
             FROM n",
            [TRAFFIC_ROWS],
        )
        .unwrap();
}

/// DB-01: the Traffic page's export of a full log is read and formatted
/// off the async workers. Both ran on the async worker before, keeping
/// the runtime busy for seconds in a debug build.
#[tokio::test]
async fn traffic_export_of_a_full_log_does_not_hold_the_runtime() {
    let (h, c, _) = signed_in();
    seed_traffic(&h);
    let app = app(&h);
    let ((s, csv), busy) = crate::runtime_busy(fetch(&h, &app, &c, "/traffic/export")).await;
    assert_eq!(s, StatusCode::OK);
    let csv = String::from_utf8(csv).unwrap();
    assert!(csv.starts_with("Timestamp,Client IP,Method,Path"));
    assert!(
        csv.lines().count() as i64 > TRAFFIC_ROWS,
        "header and every request"
    );
    assert!(
        busy < Duration::from_millis(500),
        "the export kept the runtime busy for {:?}",
        busy
    );
}

/// The most logs connections in use at once, sampled every 100 µs on a
/// thread of its own until `stop`.
fn logs_in_use_probe(ctx: Arc<AppState>, stop: Arc<AtomicBool>) -> JoinHandle<u32> {
    std::thread::spawn(move || {
        let mut most = 0;
        while !stop.load(Ordering::Relaxed) {
            let (open, idle) = ctx.logs.pool_state();
            most = most.max(open - idle);
            std::thread::sleep(Duration::from_micros(100));
        }
        most
    })
}

/// Four Traffic page refreshes at once share one computation of the
/// statistics (single flight), which holds one logs connection. Computed
/// apiece on the blocking pool, the four held all four of the logs pool's
/// connections for 19 scans of a full log each, leaving none to the
/// traffic, latency, error and order log writers.
#[tokio::test]
async fn traffic_stats_refreshes_share_one_computation() {
    let (h, c, _) = signed_in();
    seed_traffic(&h);
    let app = app(&h);
    let stop = Arc::new(AtomicBool::new(false));
    let probe = logs_in_use_probe(h.ctx.clone(), stop.clone());
    let stats = || fetch(&h, &app, &c, "/traffic/api/stats");
    let (a, b, d, e) = tokio::join!(stats(), stats(), stats(), stats());
    stop.store(true, Ordering::Relaxed);
    let most = probe.join().unwrap();
    for (s, body) in [a, b, d, e] {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(s, StatusCode::OK, "{}", v);
        assert_eq!(v["overall"]["total_requests"], TRAFFIC_ROWS);
    }
    assert_eq!(most, 1, "logs connections held at once by the refreshes");
}

/// Order log entries for the export test, all on the harness's day.
const ORDER_LOG_ROWS: i64 = 50_000;

/// `ORDER_LOG_ROWS` placed orders in one statement, within the last 8 hours.
fn seed_order_logs(h: &H) {
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO order_logs (api_type, request_data, response_data, created_at)
             SELECT 'placeorder',
                 json_object('strategy', 's1', 'exchange', 'NSE', 'symbol', 'SBIN',
                     'action', 'BUY', 'product', 'MIS', 'pricetype', 'MARKET', 'quantity', '1'),
                 json_object('status', 'success', 'orderid', 'ord' || i),
                 strftime('%Y-%m-%dT%H:%M:%SZ', '2026-10-05 04:00:00',
                     '-' || (i % 30000) || ' seconds')
             FROM n",
            [ORDER_LOG_ROWS],
        )
        .unwrap();
}

/// DB-01: the Logs page's export of a large day is read and formatted off
/// the async workers. Both ran on the async worker before (each entry's
/// request and response parsed there), keeping the runtime busy for
/// seconds in a debug build.
#[tokio::test]
async fn order_log_export_of_a_large_day_does_not_hold_the_runtime() {
    let (h, c, _) = signed_in();
    seed_order_logs(&h);
    let app = app(&h);
    let path = "/logs/export?start_date=2026-10-05&end_date=2026-10-05";
    let ((s, csv), busy) = crate::runtime_busy(fetch(&h, &app, &c, path)).await;
    assert_eq!(s, StatusCode::OK);
    let csv = String::from_utf8(csv).unwrap();
    assert!(csv.starts_with("ID,Timestamp,API Type,Strategy"));
    assert_eq!(
        csv.lines().count() as i64,
        ORDER_LOG_ROWS + 1,
        "header and every entry"
    );
    assert!(
        busy < Duration::from_millis(500),
        "the export kept the runtime busy for {:?}",
        busy
    );
}

/// A week of health samples, one a minute (`health_service::SAMPLE_EVERY`).
const HEALTH_SAMPLES: i64 = 7 * 24 * 60;

fn seed_health(h: &H) {
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO health_metrics (timestamp, fd_count, fd_limit, fd_usage_percent,
                 fd_status, memory_rss_mb, memory_status, db_connections_total, db_status,
                 ws_connections_total, ws_status, thread_count, thread_status, db_sizes,
                 overall_status)
             SELECT datetime('2026-10-05 04:00:00', '-' || i || ' minutes'), 40 + i % 9, 10240,
                 0.4, 'pass', 180.0 + i % 50, 'pass', 3, 'pass', 1, 'pass', 30 + i % 5, 'pass',
                 json_object('openalgo', 2.5, 'logs', 40.0 + i * 0.001, 'sandbox', 1.0,
                     'historify', 8.0),
                 'pass'
             FROM n",
            [HEALTH_SAMPLES],
        )
        .unwrap();
}

/// Requests the Analyzer page lists at most (`analyzer_log_service::MAX_ROWS`).
const ANALYZER_ROWS: i64 = 10_000;

/// `ANALYZER_ROWS` sandbox requests in one statement, on the harness's day.
fn seed_analyzer(h: &H) {
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO analyzer_logs (api_type, request_data, response_data, created_at)
             SELECT 'placeorder',
                 json_object('strategy', 's1', 'exchange', 'NSE', 'symbol', 'SBIN',
                     'action', 'BUY', 'quantity', '1'),
                 json_object('status', 'success', 'orderid', 'sb' || i),
                 strftime('%Y-%m-%dT%H:%M:%SZ', '2026-10-05 03:30:00', '-' || i || ' seconds')
             FROM n",
            [ANALYZER_ROWS],
        )
        .unwrap();
}

/// The report pages a trader keeps open, as their pages call them: the
/// Traffic page (refreshed every 30 s), the Logs page (a page and a search
/// over every entry), the Health page's week of history and statistics,
/// and the Analyzer page and its export.
const REPORT_PAGES: &[&str] = &[
    "/traffic/api/logs?limit=1000",
    "/traffic/api/stats",
    "/logs?page=1",
    "/logs?page=1&search=ord4",
    "/health/api/history?hours=168",
    "/health/api/stats?hours=168",
    "/analyzer/api/data",
    "/analyzer/export",
];

/// Async-worker DB reads (roadmap): the report pages over a large log never
/// hold the runtime, two of each at once. Each read SQLite and shaped its
/// answer on the async worker, so a full traffic log, a large order log or
/// a week of health samples held the runtime that carries the market data
/// feed and the risk owners for the length of the query (2.1-2.9 s at once
/// in a debug build). Serializing the largest answers there (a week of
/// samples, 10,000 analyzer requests) still held it for up to 0.46 s on a
/// loaded machine, so they are serialized where they are read.
///
/// The hold is the longest the runtime thread ran without going idle. A
/// 10 ms timer beside the pages ([`crate::runtime_stall`]) once ran 374 ms
/// late on a CI runner while every page's polls took 11.8 ms in all: the
/// runner had not run the idle thread, which no page can change.
///
/// Sixteen pages built at once on the blocking pool kept every core of a
/// three-core macOS runner busy, and the runtime thread, woken, waited for
/// a core: 244-317 ms held while the pages' polls took 19 ms (CI 40716174457,
/// 40722674764), 94-549 ms on a Mac's four efficiency cores (`taskpolicy
/// -b`). The log reports now take turns, two at once (`AppState::report`):
/// 4-13 ms there.
#[test]
fn report_pages_over_a_large_log_do_not_hold_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let (h, c, _) = signed_in();
        seed_traffic(&h);
        seed_order_logs(&h);
        seed_health(&h);
        seed_analyzer(&h);
        let app = app(&h);
        let pages: Vec<&str> = REPORT_PAGES.iter().chain(REPORT_PAGES).copied().collect();
        let each = pages
            .iter()
            .map(|p| crate::runtime_busy(fetch(&h, &app, &c, p)));
        let (replies, held) = runtime
            .longest_hold(futures_util::future::join_all(each))
            .await;
        for (p, ((s, body), _)) in pages.iter().zip(&replies) {
            assert_eq!(
                *s,
                StatusCode::OK,
                "{}: {}",
                p,
                String::from_utf8_lossy(body)
            );
        }
        let body = |i: usize| &replies[i].0 .1;
        let json = |i: usize| -> serde_json::Value { serde_json::from_slice(body(i)).unwrap() };
        assert_eq!(json(0).as_array().unwrap().len(), 1000);
        assert_eq!(json(1)["overall"]["total_requests"], TRAFFIC_ROWS);
        assert_eq!(json(2)["logs"].as_array().unwrap().len(), 20);
        assert!(json(3)["logs"].as_array().is_some_and(|l| !l.is_empty()));
        assert!(json(4).as_array().unwrap().len() as i64 >= HEALTH_SAMPLES - 60);
        assert!(json(5)["total_samples"].as_i64().unwrap() >= HEALTH_SAMPLES - 60);
        let listed = json(6)["data"]["requests"].as_array().unwrap().len();
        assert_eq!(listed as i64, ANALYZER_ROWS);
        let csv = String::from_utf8_lossy(body(7)).into_owned();
        assert_eq!(
            csv.lines().count() as i64,
            ANALYZER_ROWS + 1,
            "header and every request"
        );
        // Each page's polls on the runtime, for the next reader of a failure.
        let busy: Duration = replies.iter().map(|r| r.1).sum();
        let by_page: Vec<String> = pages
            .iter()
            .zip(&replies)
            .map(|(p, r)| format!("{} {:?}", p, r.1))
            .collect();
        let by_page = by_page.join(", ");
        eprintln!(
            "report pages: the runtime was held for {:?} at most, busy {:?} in all ({})",
            held, busy, by_page
        );
        assert!(
            held < Duration::from_millis(200),
            "the runtime was held for {:?} at once ({})",
            held,
            by_page
        );
        assert!(
            busy < Duration::from_secs(1),
            "the pages kept the runtime busy for {:?} in all ({})",
            busy,
            by_page
        );
    });
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
