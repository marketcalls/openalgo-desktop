//! Historify: schema migration on populated files, the download job engine
//! against the scriptable broker, the scheduler on an injected clock,
//! import/export round trips, every `/historify/api` route (access, CSRF,
//! shapes, validation), `/api/v1/history?source=db`, and resource hygiene.

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::{MockBroker, MockCall};
use openalgo_desktop_lib::brokers::types::{AuthToken, Candle, HistoryRequest, QuoteKey};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::{Clock, ManualClock};
use openalgo_desktop_lib::db::duckdb::HistorifyDb;
use openalgo_desktop_lib::historify::export::{self, ExportSpec};
use openalgo_desktop_lib::historify::import::{self, FileKind};
use openalgo_desktop_lib::historify::jobs::{CreateJob, EngineConfig, RETRY_BUSY_MESSAGE};
use openalgo_desktop_lib::historify::scheduler::AddSchedule;
use openalgo_desktop_lib::historify::source::{HistorySource, Notifier};
use openalgo_desktop_lib::historify::{db, Historify};
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::ServiceExt;

// ------------------------------------------------------------- fixtures

fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

/// 2024-01-01 09:15 IST.
const OPEN_TS: i64 = 1_704_080_700;

fn minute_bars(n: i64) -> Vec<Candle> {
    (0..n)
        .map(|i| Candle {
            timestamp: OPEN_TS + i * 60,
            open: 100.0 + i as f64,
            high: 101.5 + i as f64,
            low: 99.25 + i as f64,
            close: 100.5 + i as f64,
            volume: 1000 + i,
            oi: 0,
        })
        .collect()
}

fn daily_bars(n: i64) -> Vec<Candle> {
    let day0 = 1_704_047_400; // 2024-01-01 00:00 IST
    (0..n)
        .map(|i| Candle {
            timestamp: day0 + i * 86_400,
            open: 10.0,
            high: 12.0,
            low: 9.0,
            close: 11.0 + i as f64,
            volume: 100,
            oi: 5,
        })
        .collect()
}

fn sym(symbol: &str, exchange: &str, name: &str, expiry: &str, strike: f64, it: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: format!("T{}{}", symbol, exchange),
        expiry: expiry.into(),
        strike,
        lot_size: 1,
        instrument_type: it.into(),
        tick_size: 0.05,
    }
}

fn master() -> Vec<SymToken> {
    let mut v = Vec::new();
    for s in ["SBIN", "INFY", "TCS", "RELIANCE", "ITC", "HDFCBANK"] {
        v.push(sym(s, "NSE", s, "", 0.0, "EQ"));
    }
    v.push(sym(
        "NIFTY28OCT2625000CE",
        "NFO",
        "NIFTY",
        "28-OCT-26",
        25000.0,
        "CE",
    ));
    v.push(sym(
        "NIFTY28OCT2625000PE",
        "NFO",
        "NIFTY",
        "28-OCT-26",
        25000.0,
        "PE",
    ));
    v.push(sym(
        "NIFTY28OCT2625100CE",
        "NFO",
        "NIFTY",
        "28-OCT-26",
        25100.0,
        "CE",
    ));
    v.push(sym(
        "NIFTY25NOV2625000CE",
        "NFO",
        "NIFTY",
        "25-NOV-26",
        25000.0,
        "CE",
    ));
    v.push(sym(
        "NIFTY28OCT26FUT",
        "NFO",
        "NIFTY",
        "28-OCT-26",
        0.0,
        "FUT",
    ));
    v.push(sym(
        "011NSETEST28OCT26FUT",
        "NFO",
        "011NSETEST",
        "28-OCT-26",
        0.0,
        "FUT",
    ));
    v
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
}

impl Notifier for Recorder {
    fn notify(&self, event: &'static str, payload: Value) {
        self.events.lock().push((event.to_string(), payload));
    }
}

impl Recorder {
    fn named(&self, name: &str) -> Vec<Value> {
        self.events
            .lock()
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    async fn wait(&self, name: &str, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..1000 {
            if let Some(v) = self.named(name).into_iter().find(|v| pred(v)) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no {} event; got {:?}", name, self.events.lock());
    }
}

/// The scriptable broker behind the job engine, with an optional gate that
/// holds every fetch until the test releases it.
struct MockSource {
    broker: Arc<MockBroker>,
    gate: Option<Arc<Semaphore>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    started: AtomicUsize,
    ready: Mutex<Result<(), String>>,
}

impl MockSource {
    fn new(broker: Arc<MockBroker>, gated: bool) -> Arc<Self> {
        Arc::new(Self {
            broker,
            gate: gated.then(|| Arc::new(Semaphore::new(0))),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            ready: Mutex::new(Ok(())),
        })
    }

    fn release(&self, n: usize) {
        if let Some(g) = &self.gate {
            g.add_permits(n);
        }
    }

    async fn wait_started(&self, n: usize) {
        for _ in 0..1000 {
            if self.started.load(Ordering::SeqCst) >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "only {} fetches started",
            self.started.load(Ordering::SeqCst)
        );
    }
}

struct InFlight<'a>(&'a AtomicUsize);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl HistorySource for MockSource {
    fn ready(&self) -> Result<(), String> {
        self.ready.lock().clone()
    }

    async fn fetch(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Candle>, String> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        let _g = InFlight(&self.in_flight);
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.started.fetch_add(1, Ordering::SeqCst);
        if let Some(g) = &self.gate {
            g.acquire().await.map_err(|e| e.to_string())?.forget();
        }
        self.broker
            .get_history(
                &AuthToken::new("mock"),
                &HistoryRequest {
                    key: QuoteKey::new(exchange, symbol),
                    interval: interval.into(),
                    start,
                    end,
                },
            )
            .await
            .map_err(|e| e.client_message())
    }
}

struct Engine {
    h: Historify,
    src: Arc<MockSource>,
    mock: Arc<MockBroker>,
    rec: Arc<Recorder>,
    clock: Arc<ManualClock>,
    dir: tempfile::TempDir,
}

fn open_engine(
    dir: tempfile::TempDir,
    gated: bool,
    cfg: EngineConfig,
    now: DateTime<Utc>,
) -> Engine {
    let mock = Arc::new(MockBroker::new("mock"));
    *mock.history.lock() = Some(Ok(daily_bars(3)));
    let src = MockSource::new(mock.clone(), gated);
    let rec = Arc::new(Recorder::default());
    let clock = ManualClock::new(now);
    let db = HistorifyDb::new(&dir.path().join("historify.duckdb")).unwrap();
    let h = Historify::new(
        db,
        src.clone(),
        rec.clone(),
        clock.clone() as Arc<dyn Clock>,
        &dir.path().join("work"),
        cfg,
    );
    Engine {
        h,
        src,
        mock,
        rec,
        clock,
        dir,
    }
}

fn engine(gated: bool) -> Engine {
    open_engine(
        tempfile::tempdir().unwrap(),
        gated,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 10, 0),
    )
}

fn job(symbols: &[&str]) -> CreateJob {
    CreateJob {
        job_type: "custom".into(),
        symbols: symbols
            .iter()
            .map(|s| (s.to_string(), "NSE".to_string()))
            .collect(),
        interval: "D".into(),
        start_date: Some("2024-01-01".into()),
        end_date: Some("2024-01-10".into()),
        config: json!({}),
        incremental: false,
    }
}

async fn job_row(e: &Engine, id: &str) -> Value {
    e.h.jobs.status(id).await.body["job"].clone()
}

async fn wait_status(e: &Engine, id: &str, status: &str) -> Value {
    for _ in 0..1000 {
        let j = job_row(e, id).await;
        if j["status"] == status {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "job {} never reached {}: {}",
        id,
        status,
        e.h.jobs.status(id).await.body
    );
}

async fn wait_idle(e: &Engine) {
    for _ in 0..1000 {
        if e.h.jobs.active_jobs() == 0
            && e.h.jobs.task_count() == 0
            && e.h.db.open_connections() == 0
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "engine not idle: {} jobs, {} tasks, {} connections",
        e.h.jobs.active_jobs(),
        e.h.jobs.task_count(),
        e.h.db.open_connections()
    );
}

fn history_calls(m: &MockBroker, symbol: &str) -> usize {
    m.calls()
        .iter()
        .filter(|c| matches!(c, MockCall::History(r) if r.key.symbol == symbol))
        .count()
}

// ----------------------------------------------------- schema migration

/// The tables earlier desktop builds created.
const DESKTOP_V1: &str = r#"
CREATE TABLE migrations (name VARCHAR PRIMARY KEY, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
INSERT INTO migrations (name) VALUES ('001_market_data'), ('002_watchlist'), ('003_data_catalog'),
    ('004_download_jobs'), ('005_symbol_metadata');
CREATE TABLE market_data (
    symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL, timeframe VARCHAR NOT NULL,
    timestamp TIMESTAMP NOT NULL, open DOUBLE NOT NULL, high DOUBLE NOT NULL, low DOUBLE NOT NULL,
    close DOUBLE NOT NULL, volume BIGINT NOT NULL,
    PRIMARY KEY (symbol, exchange, timeframe, timestamp));
CREATE INDEX idx_market_data_symbol ON market_data(symbol, exchange);
CREATE INDEX idx_market_data_timestamp ON market_data(timestamp);
CREATE TABLE watchlist (
    id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL, name VARCHAR NOT NULL,
    list_name VARCHAR NOT NULL DEFAULT 'default', order_index INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, UNIQUE (symbol, exchange, list_name));
CREATE TABLE data_catalog (
    id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL,
    timeframe VARCHAR NOT NULL, from_date DATE NOT NULL, to_date DATE NOT NULL,
    row_count BIGINT NOT NULL DEFAULT 0, last_updated TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (symbol, exchange, timeframe));
CREATE TABLE download_jobs (
    id INTEGER PRIMARY KEY, name VARCHAR NOT NULL, status VARCHAR NOT NULL DEFAULT 'pending',
    total_items INTEGER NOT NULL DEFAULT 0, completed_items INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, completed_at TIMESTAMP);
CREATE TABLE job_items (
    id INTEGER PRIMARY KEY, job_id INTEGER NOT NULL REFERENCES download_jobs(id),
    symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL, timeframe VARCHAR NOT NULL,
    status VARCHAR NOT NULL DEFAULT 'pending', error VARCHAR);
CREATE TABLE symbol_metadata (
    symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL, name VARCHAR NOT NULL, sector VARCHAR,
    industry VARCHAR, market_cap DOUBLE, updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (symbol, exchange));

INSERT INTO market_data VALUES
    ('SBIN', 'NSE', 'D', TIMESTAMP '2024-01-01 00:00:00', 1, 2, 0.5, 1.5, 100),
    ('SBIN', 'NSE', 'D', TIMESTAMP '2024-01-02 00:00:00', 2, 3, 1.5, 2.5, 200),
    ('INFY', 'NSE', '1m', TIMESTAMP '2024-01-01 03:45:00', 10, 11, 9, 10.5, 50);
INSERT INTO watchlist (id, symbol, exchange, name, list_name) VALUES
    (7, 'SBIN', 'NSE', 'State Bank', 'default'), (9, 'SBIN', 'NSE', 'SBI', 'other'),
    (12, 'INFY', 'NSE', 'Infosys', 'default');
INSERT INTO data_catalog (id, symbol, exchange, timeframe, from_date, to_date, row_count, last_updated)
    VALUES (1, 'SBIN', 'NSE', 'D', DATE '2024-01-01', DATE '2024-01-02', 2, TIMESTAMP '2024-01-03 10:00:00');
INSERT INTO download_jobs (id, name, status, total_items, completed_items) VALUES (3, 'old', 'completed', 2, 2),
    (4, 'stuck', 'running', 1, 0);
INSERT INTO job_items (id, job_id, symbol, exchange, timeframe, status, error) VALUES
    (1, 3, 'SBIN', 'NSE', 'D', 'completed', NULL), (2, 3, 'INFY', 'NSE', 'D', 'failed', 'boom'),
    (3, 4, 'TCS', 'NSE', 'D', 'pending', NULL);
INSERT INTO symbol_metadata (symbol, exchange, name, sector) VALUES ('SBIN', 'NSE', 'State Bank', 'Banks');
"#;

#[test]
fn desktop_v1_file_is_converted_to_the_web_schema_with_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("historify.duckdb");
    {
        let c = duckdb::Connection::open(&path).unwrap();
        c.execute_batch(DESKTOP_V1).unwrap();
    }
    for _ in 0..2 {
        let db = HistorifyDb::new(&path).unwrap();
        db.read(|c| {
            let bars = db::stored(c, "SBIN", "NSE", "D", None, None)?;
            assert_eq!(bars.len(), 2);
            assert_eq!(bars[0].timestamp, 1_704_067_200);
            assert_eq!(bars[1].close, 2.5);
            assert_eq!(
                db::stored(c, "INFY", "NSE", "1m", None, None)?[0].timestamp,
                OPEN_TS
            );
            let cat = db::catalog(c)?;
            assert_eq!(cat.len(), 2);
            let sbin = cat.iter().find(|r| r["symbol"] == "SBIN").unwrap();
            assert_eq!(sbin["record_count"], 2);
            assert_eq!(sbin["last_download_at"], "Wed, 03 Jan 2024 10:00:00 GMT");
            let w: Vec<Value> = db::watchlist(c)?
                .into_iter()
                .filter(|r| r["symbol"] != "TCS")
                .collect();
            assert_eq!(w.len(), 2, "one row per symbol and exchange: {:?}", w);
            let sb = w.iter().find(|r| r["symbol"] == "SBIN").unwrap();
            assert_eq!(sb["id"], 7);
            assert_eq!(sb["display_name"], "State Bank");
            let jobs = db::jobs(c, None, 50)?;
            assert_eq!(jobs.len(), 2);
            let old = db::job(c, "3")?.unwrap();
            assert_eq!(old["total_symbols"], 2);
            assert_eq!(old["status"], "completed");
            assert_eq!(db::job(c, "4")?.unwrap()["status"], "failed");
            let items = db::job_items(c, "3", None)?;
            assert_eq!(items[0]["status"], "success");
            assert_eq!(items[1]["status"], "error");
            assert_eq!(items[1]["error_message"], "boom");
            Ok(())
        })
        .unwrap();
        // Sequences start past the carried-over ids.
        db.mutate(|c| {
            db::watchlist_add(
                c,
                "TCS",
                "NSE",
                None,
                NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )?;
            Ok(())
        })
        .unwrap();
        db.close();
    }
    let db = HistorifyDb::new(&path).unwrap();
    db.mutate(|c| {
        db::watchlist_add(
            c,
            "ITC",
            "NSE",
            None,
            NaiveDate::from_ymd_opt(2026, 1, 2)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        )?;
        Ok(())
    })
    .unwrap();
    let mut ids: Vec<i64> = db
        .read(|c| {
            Ok(db::watchlist(c)?
                .iter()
                .map(|r| r["id"].as_i64().unwrap())
                .collect())
        })
        .unwrap();
    ids.sort();
    assert_eq!(ids, vec![7, 12, 13, 14]);
}

#[test]
fn web_created_file_is_left_as_is_and_lagging_sequences_advance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("historify.duckdb");
    {
        // A file from an older web release: no `oi`, no scheduler tables,
        // rows inserted by a build that assigned MAX(id) + 1 itself.
        let c = duckdb::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE market_data (symbol VARCHAR NOT NULL, exchange VARCHAR NOT NULL,
                interval VARCHAR NOT NULL, timestamp BIGINT NOT NULL, open DOUBLE NOT NULL,
                high DOUBLE NOT NULL, low DOUBLE NOT NULL, close DOUBLE NOT NULL,
                volume BIGINT NOT NULL, PRIMARY KEY (symbol, exchange, interval, timestamp));
             INSERT INTO market_data VALUES ('SBIN', 'NSE', 'D', 1704047400, 1, 2, 0.5, 1.5, 10);
             CREATE TABLE watchlist (id INTEGER PRIMARY KEY, symbol VARCHAR NOT NULL,
                exchange VARCHAR NOT NULL, display_name VARCHAR,
                added_at TIMESTAMP DEFAULT current_timestamp, UNIQUE (symbol, exchange));
             CREATE SEQUENCE watchlist_id_seq START 1;
             INSERT INTO watchlist (id, symbol, exchange) VALUES (5, 'SBIN', 'NSE');",
        )
        .unwrap();
    }
    let db = HistorifyDb::new(&path).unwrap();
    db.read(|c| {
        let bars = db::stored(c, "SBIN", "NSE", "D", None, None)?;
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].oi, 0);
        assert!(db::schedules(c, false)?.is_empty());
        Ok(())
    })
    .unwrap();
    db.mutate(|c| {
        db::watchlist_add(
            c,
            "INFY",
            "NSE",
            None,
            NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        )?;
        Ok(())
    })
    .unwrap();
    let ids: Vec<i64> = db
        .read(|c| {
            Ok(db::watchlist(c)?
                .iter()
                .map(|r| r["id"].as_i64().unwrap())
                .collect())
        })
        .unwrap();
    assert!(ids.contains(&6), "{:?}", ids);
    assert_eq!(db.open_connections(), 0);
}

// ------------------------------------------------------------ job engine

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_emits_progress_and_completes_like_the_web() {
    let e = engine(false);
    let r =
        e.h.jobs
            .create_and_start(job(&["SBIN", "INFY", "TCS"]))
            .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["message"], "Job started with 3 symbols");
    assert_eq!(r.body["total_symbols"], 3);
    assert_eq!(r.body["incremental"], false);
    let id = r.body["job_id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 8);
    let done = e
        .rec
        .wait("historify_job_complete", |v| v["job_id"] == id.as_str())
        .await;
    assert_eq!(
        done,
        json!({"job_id": id, "completed": 3, "failed": 0, "total": 3, "status": "completed"})
    );
    let progress = e.rec.named("historify_progress");
    assert_eq!(progress.len(), 3);
    assert_eq!(progress[0]["current"], 1);
    assert_eq!(progress[0]["symbol"], "SBIN");
    assert_eq!(progress[0]["percent"], 33.3);
    assert_eq!(progress[2]["percent"], 100.0);
    let j = wait_status(&e, &id, "completed").await;
    assert_eq!(j["completed_symbols"], 3);
    assert_eq!(j["failed_symbols"], 0);
    assert!(j["started_at"].is_string() && j["completed_at"].is_string());
    assert_eq!(j["config"], r#"{"incremental":false}"#);
    let items = e.h.jobs.status(&id).await.body["items"].clone();
    for it in items.as_array().unwrap() {
        assert_eq!(it["status"], "success");
        assert_eq!(it["records_downloaded"], 3);
    }
    let cat = e.h.catalog().await.body;
    assert_eq!(cat["count"], 3);
    wait_idle(&e).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_items_are_retried_once_and_completion_is_exactly_once() {
    let e = engine(false);
    *e.mock.history.lock() = Some(Err("Token is invalid or expired".into()));
    let id = e.h.jobs.create_and_start(job(&["SBIN", "INFY"])).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let done = e
        .rec
        .wait("historify_job_complete", |v| v["job_id"] == id.as_str())
        .await;
    assert_eq!(done["status"], "completed_with_errors");
    assert_eq!(done["failed"], 2);
    let j = wait_status(&e, &id, "completed_with_errors").await;
    assert_eq!(j["failed_symbols"], 2);
    let items = e.h.jobs.status(&id).await.body["items"].clone();
    assert_eq!(items[0]["error_message"], "Token is invalid or expired");
    wait_idle(&e).await;

    // Interval that cannot be downloaded fails per item with the web's text.
    let mut bad = job(&["SBIN"]);
    bad.interval = "5m".into();
    let bid = e.h.jobs.create_and_start(bad).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_status(&e, &bid, "completed_with_errors").await;
    assert_eq!(
        e.h.jobs.status(&bid).await.body["items"][0]["error_message"],
        "Only 1m, D intervals can be downloaded. Other timeframes (15m, 1h, 30m, 5m) are computed from 1m data."
    );
    wait_idle(&e).await;

    // Two retries at once: one claims the job, the other is refused.
    *e.mock.history.lock() = Some(Ok(daily_bars(2)));
    let (a, b) = tokio::join!(e.h.jobs.retry(&id), e.h.jobs.retry(&id));
    let mut statuses = vec![a.status, b.status];
    statuses.sort();
    assert!(
        statuses == vec![200, 409] || statuses == vec![200, 400],
        "{:?} {:?}",
        a,
        b
    );
    let winner = if a.status == 200 { &a } else { &b };
    assert_eq!(winner.body["retry_count"], 2);
    assert_eq!(winner.body["message"], "Retrying 2 failed items");
    let loser = if a.status == 200 { &b } else { &a };
    assert!(
        loser.message() == RETRY_BUSY_MESSAGE
            || loser.message().starts_with("Job is already running"),
        "{:?}",
        loser
    );
    let j = wait_status(&e, &id, "completed").await;
    assert_eq!(j["completed_symbols"], 2);
    assert_eq!(j["failed_symbols"], 0);
    wait_idle(&e).await;
    // Each symbol: one failed fetch, one retried fetch.
    assert_eq!(history_calls(&e.mock, "SBIN"), 2);
    assert_eq!(history_calls(&e.mock, "INFY"), 2);
    let nothing = e.h.jobs.retry(&id).await;
    assert_eq!(nothing.body["message"], "No failed items to retry");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_and_resume_hold_the_job_between_symbols() {
    let e = engine(true);
    let id =
        e.h.jobs
            .create_and_start(job(&["SBIN", "INFY", "TCS"]))
            .await
            .body["job_id"]
            .as_str()
            .unwrap()
            .to_string();
    e.src.wait_started(1).await;
    wait_status(&e, &id, "running").await;
    let p = e.h.jobs.pause(&id).await;
    assert_eq!(
        p.body,
        json!({"status": "success", "message": "Job paused"})
    );
    let again = e.h.jobs.pause(&id).await;
    assert_eq!(again.status, 400);
    assert_eq!(again.message(), "Job is not running (status: paused)");
    // The symbol in flight finishes; the next one does not start.
    e.src.release(1);
    let paused = e
        .rec
        .wait("historify_job_paused", |v| v["job_id"] == id.as_str())
        .await;
    assert_eq!(
        paused,
        json!({"job_id": id, "current": 1, "total": 3, "status": "paused"})
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(e.src.started.load(Ordering::SeqCst), 1);
    assert_eq!(job_row(&e, &id).await["status"], "paused");
    assert!(
        e.rec.named("historify_job_paused").len() >= 2,
        "heartbeat repeats"
    );

    let r = e.h.jobs.resume(&id).await;
    assert_eq!(
        r.body,
        json!({"status": "success", "message": "Job resumed"})
    );
    let not_paused = e.h.jobs.resume(&id).await;
    assert_eq!(not_paused.status, 400);
    e.src.release(2);
    let done = e
        .rec
        .wait("historify_job_complete", |v| v["job_id"] == id.as_str())
        .await;
    assert_eq!(done["completed"], 3);
    for s in ["SBIN", "INFY", "TCS"] {
        assert_eq!(history_calls(&e.mock, s), 1, "{}", s);
    }
    wait_idle(&e).await;
    assert_eq!(
        e.h.jobs.pause(&id).await.message(),
        "Job is not running (status: completed)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_stops_the_task_and_leaves_nothing_open() {
    let e = engine(true);
    let id = e.h.jobs.create_and_start(job(&["SBIN", "INFY"])).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    e.src.wait_started(1).await;
    let c = e.h.jobs.cancel(&id).await;
    assert_eq!(
        c.body,
        json!({"status": "success", "message": "Job cancelled"})
    );
    let ev = e
        .rec
        .wait("historify_job_cancelled", |v| v["job_id"] == id.as_str())
        .await;
    assert_eq!(ev, json!({"job_id": id, "status": "cancelled"}));
    wait_idle(&e).await;
    let st = e.h.jobs.status(&id).await.body;
    assert_eq!(st["job"]["status"], "cancelled");
    // The interrupted symbol is left for a retry, not marked done.
    assert_eq!(st["items"][0]["status"], "pending");
    assert!(e.rec.named("historify_job_complete").is_empty());
    let again = e.h.jobs.cancel(&id).await;
    assert_eq!(again.status, 400);
    assert_eq!(
        again.message(),
        "Job is not running or paused (status: cancelled)"
    );
    assert_eq!(e.h.jobs.delete(&id).await.status, 200);
    assert_eq!(e.h.jobs.status(&id).await.status, 404);
    assert_eq!(e.h.jobs.pause("nope").await.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn concurrency_is_bounded_across_jobs() {
    let cfg = EngineConfig {
        max_concurrent_jobs: 2,
        ..EngineConfig::immediate()
    };
    let e = open_engine(
        tempfile::tempdir().unwrap(),
        true,
        cfg,
        ist(2026, 10, 7, 10, 0),
    );
    let mut ids = Vec::new();
    for i in 0..5 {
        let s = ["SBIN", "INFY", "TCS", "ITC", "RELIANCE"][i];
        ids.push(
            e.h.jobs.create_and_start(job(&[s])).await.body["job_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    e.src.wait_started(2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(e.src.started.load(Ordering::SeqCst), 2);
    let queued = ids.len()
        - futures_util::future::join_all(ids.iter().map(|id| job_row(&e, id)))
            .await
            .iter()
            .filter(|j| j["status"] == "running")
            .count();
    assert_eq!(queued, 3);
    // A queued job can be cancelled before it starts.
    let last = ids.pop().unwrap();
    assert_eq!(e.h.jobs.cancel(&last).await.status, 200);
    e.src.release(10);
    for id in &ids {
        wait_status(&e, id, "completed").await;
    }
    assert_eq!(job_row(&e, &last).await["status"], "cancelled");
    assert_eq!(e.src.max_in_flight.load(Ordering::SeqCst), 2);
    wait_idle(&e).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_interrupted_by_shutdown_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let e = open_engine(
        dir,
        true,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 10, 0),
    );
    let id =
        e.h.jobs
            .create_and_start(job(&["SBIN", "INFY", "TCS", "ITC"]))
            .await
            .body["job_id"]
            .as_str()
            .unwrap()
            .to_string();
    e.src.release(2);
    e.src.wait_started(3).await;
    // Two symbols saved, the third in flight when the app stops.
    e.h.shutdown().await;
    assert_eq!(e.h.jobs.task_count(), 0);
    assert_eq!(e.h.db.open_connections(), 0);
    assert!(!e.h.db.is_open());
    let first_calls: Vec<usize> = ["SBIN", "INFY", "TCS", "ITC"]
        .iter()
        .map(|s| history_calls(&e.mock, s))
        .collect();
    let Engine { dir, .. } = e;

    let e2 = open_engine(
        dir,
        false,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 11, 0),
    );
    let st = e2.h.jobs.status(&id).await.body;
    assert_eq!(st["job"]["status"], "paused");
    let statuses: Vec<&str> = st["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, vec!["success", "success", "pending", "pending"]);
    let r = e2.h.jobs.resume(&id).await;
    assert_eq!(r.status, 200, "{:?}", r);
    let done = e2
        .rec
        .wait("historify_job_complete", |v| v["job_id"] == id.as_str())
        .await;
    assert_eq!(done["completed"], 4);
    assert_eq!(done["total"], 4);
    // Saved symbols are not downloaded again.
    assert_eq!(history_calls(&e2.mock, "SBIN"), 0);
    assert_eq!(history_calls(&e2.mock, "INFY"), 0);
    assert_eq!(history_calls(&e2.mock, "TCS"), 1);
    assert_eq!(history_calls(&e2.mock, "ITC"), 1);
    assert_eq!(first_calls, vec![1, 1, 0, 0]);
    let progress = e2.rec.named("historify_progress");
    assert_eq!(progress[0]["current"], 3);
    wait_idle(&e2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_downloads_only_what_is_missing() {
    let e = engine(false);
    *e.mock.history.lock() = Some(Ok(daily_bars(10)));
    let mut j = job(&["SBIN"]);
    j.start_date = Some("2024-01-01".into());
    j.end_date = Some("2024-01-10".into());
    let id = e.h.jobs.create_and_start(j.clone()).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_status(&e, &id, "completed").await;
    wait_idle(&e).await;
    j.incremental = true;
    let id2 = e.h.jobs.create_and_start(j.clone()).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let done = e
        .rec
        .wait("historify_job_complete", |v| v["job_id"] == id2.as_str())
        .await;
    assert_eq!(done["completed"], 0);
    let items = e.h.jobs.status(&id2).await.body["items"].clone();
    assert_eq!(items[0]["status"], "skipped");
    assert_eq!(
        items[0]["error_message"],
        "Data already covers requested range"
    );
    assert_eq!(history_calls(&e.mock, "SBIN"), 1);
    wait_idle(&e).await;
    // A wider range fetches only the days before and after.
    j.start_date = Some("2023-12-25".into());
    j.end_date = Some("2024-01-15".into());
    let id3 = e.h.jobs.create_and_start(j).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_status(&e, &id3, "completed").await;
    let ranges: Vec<(NaiveDate, NaiveDate)> = e
        .mock
        .calls()
        .iter()
        .filter_map(|c| match c {
            MockCall::History(r) => Some((r.start, r.end)),
            _ => None,
        })
        .collect();
    let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
    assert_eq!(
        &ranges[1..],
        &[
            (d(2023, 12, 25), d(2023, 12, 31)),
            (d(2024, 1, 11), d(2024, 1, 15)),
        ]
    );
    wait_idle(&e).await;
}

#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hundred_job_churn_keeps_descriptors_flat() {
    // Counts this process's descriptors: runs alone in a child process.
    crate::isolated!(two_hundred_job_churn_keeps_descriptors_flat);
    let e = engine(false);
    // Warm up: first job opens the database files.
    let id = e.h.jobs.create_and_start(job(&["SBIN"])).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_status(&e, &id, "completed").await;
    wait_idle(&e).await;
    let before = open_fds();
    for batch in 0..20 {
        let mut ids = Vec::new();
        for i in 0..10 {
            let s = ["SBIN", "INFY", "TCS", "ITC", "RELIANCE"][(batch + i) % 5];
            ids.push(
                e.h.jobs.create_and_start(job(&[s])).await.body["job_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        // Cancel a couple mid-way too.
        let _ = e.h.jobs.cancel(&ids[0]).await;
        for id in &ids[1..] {
            wait_status(&e, id, "completed").await;
        }
        wait_idle(&e).await;
    }
    let after = open_fds();
    assert!(
        after <= before + 2,
        "descriptors grew: {} -> {}",
        before,
        after
    );
    assert_eq!(e.h.jobs.active_jobs(), 0);
    assert_eq!(e.h.jobs.task_count(), 0);
    assert_eq!(e.h.db.open_connections(), 0);
    let listed = e.h.jobs.list(None, 500).await.body;
    assert_eq!(listed["count"], 201);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_with_a_paused_job_leaves_no_task_or_connection() {
    let e = engine(true);
    let id = e.h.jobs.create_and_start(job(&["SBIN", "INFY"])).await.body["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    e.src.wait_started(1).await;
    e.h.jobs.pause(&id).await;
    e.src.release(1);
    e.rec.wait("historify_job_paused", |_| true).await;
    e.h.start();
    assert!(e.h.scheduler.driver_running());
    e.h.shutdown().await;
    assert!(!e.h.scheduler.driver_running());
    assert_eq!(e.h.jobs.task_count(), 0);
    assert_eq!(e.h.jobs.active_jobs(), 0);
    assert_eq!(e.h.db.open_connections(), 0);
    assert!(!e.h.db.is_open());
    let r = e.h.jobs.create_and_start(job(&["SBIN"])).await;
    assert!(r.status >= 500);
}

// --------------------------------------------------------------- scheduler

async fn add_watch(e: &Engine, symbols: &[&str]) {
    let r = SymbolResolver::new();
    r.load(master());
    for s in symbols {
        let x = e.h.add_watchlist(&r, s, "NSE", None).await;
        assert_eq!(x.status, 200, "{:?}", x);
    }
}

fn daily(at: &str) -> AddSchedule {
    AddSchedule {
        name: "Close".into(),
        schedule_type: "daily".into(),
        data_interval: "D".into(),
        interval_value: None,
        interval_unit: Some("minutes".into()),
        time_of_day: Some(at.into()),
        lookback_days: 3,
        description: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daily_schedule_fires_across_days_and_skips_late_misfires() {
    let e = open_engine(
        tempfile::tempdir().unwrap(),
        false,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 8, 0),
    );
    add_watch(&e, &["SBIN", "INFY"]).await;
    let msg = e.h.scheduler.add("sched001", daily("15:45")).await.unwrap();
    assert_eq!(msg, "Schedule 'Close' created successfully");
    assert_eq!(
        e.rec.named("historify_schedule_created"),
        vec![json!({"schedule_id": "sched001"})]
    );
    assert_eq!(
        e.h.scheduler.next_run_time("sched001"),
        Some(ist(2026, 10, 7, 15, 45))
    );
    let row = e.h.scheduler.schedule("sched001").await.unwrap().unwrap();
    assert_eq!(row["next_run_at"], "2026-10-07T15:45:00+05:30");
    assert_eq!(row["apscheduler_job_id"], "historify_schedule_sched001");
    assert_eq!(row["download_source"], "watchlist");

    assert!(e
        .h
        .scheduler
        .tick(ist(2026, 10, 7, 15, 44))
        .await
        .fired
        .is_empty());
    e.clock.set(ist(2026, 10, 7, 15, 45));
    let t = e.h.scheduler.tick(ist(2026, 10, 7, 15, 45)).await;
    assert_eq!(t.fired, vec!["sched001".to_string()]);
    let started = e
        .rec
        .wait("historify_schedule_execution_started", |_| true)
        .await;
    assert_eq!(started["schedule_id"], "sched001");
    let job_id = started["job_id"].as_str().unwrap().to_string();
    let done = e
        .rec
        .wait("historify_schedule_execution_complete", |_| true)
        .await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["execution_id"], started["execution_id"]);
    let j = wait_status(&e, &job_id, "completed").await;
    assert_eq!(j["job_type"], "scheduled");
    assert_eq!(j["start_date"], "2026-10-04");
    assert_eq!(j["end_date"], "2026-10-07");
    assert!(j["config"]
        .as_str()
        .unwrap()
        .contains(r#""incremental":true"#));
    wait_idle(&e).await;
    for _ in 0..100 {
        if e.h.scheduler.running_claims() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = e.h.scheduler.schedule("sched001").await.unwrap().unwrap();
    assert_eq!(row["total_runs"], 1);
    assert_eq!(row["successful_runs"], 1);
    assert_eq!(row["status"], "idle");
    assert_eq!(row["last_run_status"], "completed");
    assert_eq!(row["next_run_at"], "2026-10-08T15:45:00+05:30");
    let execs = e.h.db.read(|c| db::executions(c, "sched001", 10)).unwrap();
    assert_eq!(execs[0]["status"], "completed");
    assert_eq!(execs[0]["symbols_processed"], 2);
    assert_eq!(execs[0]["symbols_success"], 2);
    assert_eq!(execs[0]["download_job_id"], job_id.as_str());

    // Next day, two minutes late: inside the grace period, runs once.
    let t = e.h.scheduler.tick(ist(2026, 10, 8, 15, 47)).await;
    assert_eq!(t.fired, vec!["sched001".to_string()]);
    e.rec
        .wait("historify_schedule_execution_complete", |v| {
            v["execution_id"] != done["execution_id"]
        })
        .await;
    wait_idle(&e).await;
    for _ in 0..100 {
        if e.h.scheduler.running_claims() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Three days later (app closed): late beyond grace, skipped once, and
    // the next run is computed from now.
    let t = e.h.scheduler.tick(ist(2026, 10, 12, 10, 0)).await;
    assert!(t.fired.is_empty());
    assert_eq!(t.missed, vec!["sched001".to_string()]);
    assert_eq!(
        e.h.scheduler.next_run_time("sched001"),
        Some(ist(2026, 10, 12, 15, 45))
    );
    let row = e.h.scheduler.schedule("sched001").await.unwrap().unwrap();
    assert_eq!(row["total_runs"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interval_schedule_restores_after_restart_with_misfire_rules() {
    let dir = tempfile::tempdir().unwrap();
    let e = open_engine(
        dir,
        false,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 10, 0),
    );
    add_watch(&e, &["SBIN"]).await;
    let add = AddSchedule {
        name: "Every 15".into(),
        schedule_type: "interval".into(),
        data_interval: "1m".into(),
        interval_value: Some(15),
        interval_unit: Some("minutes".into()),
        time_of_day: Some("09:15".into()),
        lookback_days: 1,
        description: Some("intraday".into()),
    };
    e.h.scheduler.add("intv0001", add).await.unwrap();
    assert_eq!(
        e.h.scheduler.next_run_time("intv0001"),
        Some(ist(2026, 10, 7, 10, 15))
    );
    e.h.shutdown().await;
    let Engine { dir, .. } = e;

    // Reopened 3 minutes after the missed run: within grace, runs once.
    let e2 = open_engine(
        dir,
        false,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 10, 18),
    );
    assert_eq!(
        e2.h.scheduler.next_run_time("intv0001"),
        Some(ist(2026, 10, 7, 10, 15))
    );
    let t = e2.h.scheduler.tick(ist(2026, 10, 7, 10, 18)).await;
    assert_eq!(t.fired, vec!["intv0001".to_string()]);
    assert_eq!(
        e2.h.scheduler.next_run_time("intv0001"),
        Some(ist(2026, 10, 7, 10, 30))
    );
    e2.rec
        .wait("historify_schedule_execution_complete", |_| true)
        .await;
    wait_idle(&e2).await;
    e2.h.shutdown().await;
    let Engine { dir, .. } = e2;

    // Reopened an hour after the next run: beyond grace, skipped, back on
    // the 15-minute grid.
    let e3 = open_engine(
        dir,
        false,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 11, 40),
    );
    let t = e3.h.scheduler.tick(ist(2026, 10, 7, 11, 40)).await;
    assert_eq!(t.missed, vec!["intv0001".to_string()]);
    assert_eq!(
        e3.h.scheduler.next_run_time("intv0001"),
        Some(ist(2026, 10, 7, 11, 45))
    );

    // Pause holds it; resume puts it back on the grid.
    e3.h.scheduler.pause("intv0001").await.unwrap();
    assert_eq!(e3.h.scheduler.next_run_time("intv0001"), None);
    assert!(e3
        .h
        .scheduler
        .tick(ist(2026, 10, 7, 11, 46))
        .await
        .fired
        .is_empty());
    e3.clock.set(ist(2026, 10, 7, 12, 1));
    e3.h.scheduler.resume("intv0001").await.unwrap();
    assert_eq!(
        e3.h.scheduler.next_run_time("intv0001"),
        Some(ist(2026, 10, 7, 12, 15))
    );
    // Disabled schedules do not run, even when triggered.
    e3.h.scheduler.disable("intv0001").await.unwrap();
    assert_eq!(e3.h.scheduler.entry_count(), 0);
    e3.h.scheduler.trigger("intv0001").await.unwrap();
    assert!(e3
        .rec
        .named("historify_schedule_execution_started")
        .is_empty());
    e3.h.scheduler.enable("intv0001").await.unwrap();
    assert_eq!(e3.h.scheduler.entry_count(), 1);
    e3.h.scheduler.delete("intv0001").await.unwrap();
    assert_eq!(e3.h.scheduler.entry_count(), 0);
    assert_eq!(
        e3.rec.named("historify_schedule_deleted"),
        vec![json!({"schedule_id": "intv0001"})]
    );
    e3.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_runs_do_not_overlap_and_report_missing_symbols() {
    let e = open_engine(
        tempfile::tempdir().unwrap(),
        true,
        EngineConfig::immediate(),
        ist(2026, 10, 7, 9, 0),
    );
    e.h.scheduler.add("nosym001", daily("09:15")).await.unwrap();
    e.h.scheduler.trigger("nosym001").await.unwrap();
    let row = e.h.scheduler.schedule("nosym001").await.unwrap().unwrap();
    assert_eq!(row["last_run_status"], "no_symbols");
    assert_eq!(row["status"], "idle");
    assert_eq!(e.h.scheduler.running_claims(), 0);

    add_watch(&e, &["SBIN"]).await;
    *e.src.ready.lock() = Err("not connected".into());
    e.h.scheduler.trigger("nosym001").await.unwrap();
    let row = e.h.scheduler.schedule("nosym001").await.unwrap().unwrap();
    assert_eq!(row["last_run_status"], "no_broker_session");
    *e.src.ready.lock() = Ok(());

    e.h.scheduler.trigger("nosym001").await.unwrap();
    e.src.wait_started(1).await;
    assert_eq!(e.h.scheduler.running_claims(), 1);
    // A second run while the first is downloading is skipped.
    e.h.scheduler.trigger("nosym001").await.unwrap();
    assert_eq!(e.rec.named("historify_schedule_execution_started").len(), 1);
    e.src.release(5);
    e.rec
        .wait("historify_schedule_execution_complete", |_| true)
        .await;
    assert_eq!(e.h.scheduler.running_claims(), 0);
    wait_idle(&e).await;
}

// ------------------------------------------------------ import / export

fn seed(db: &HistorifyDb) {
    let now = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    db.mutate(|c| {
        db::upsert_bars(c, "SBIN", "NSE", "1m", &minute_bars(30), now)?;
        db::upsert_bars(c, "SBIN", "NSE", "D", &daily_bars(10), now)?;
        db::upsert_bars(c, "INFY", "NSE", "D", &daily_bars(4), now)?;
        Ok(())
    })
    .unwrap();
}

fn import_into(db: &HistorifyDb, path: &Path, kind: FileKind, symbol: &str, iv: &str) -> usize {
    let parsed = db
        .read(|c| import::read_file(c, path, kind))
        .unwrap()
        .unwrap();
    let now = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    db.mutate(|c| db::upsert_bars(c, symbol, "NSE", iv, &parsed.bars, now))
        .unwrap()
}

#[test]
fn csv_zip_and_parquet_exports_import_back_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = HistorifyDb::new(&dir.path().join("h.duckdb")).unwrap();
    seed(&db);
    let original = db
        .read(|c| db::stored(c, "SBIN", "NSE", "1m", None, None))
        .unwrap();

    // ZIP of one CSV per interval: 1m back in as-is, 5m aggregated.
    let zip_path = dir.path().join("x.zip");
    let spec = ExportSpec {
        symbols: vec![("SBIN".into(), "NSE".into())],
        intervals: vec!["1m".into(), "5m".into(), "W".into()],
        ..Default::default()
    };
    let (msg, n) = db
        .read(|c| export::export_zip(c, &zip_path, &spec))
        .unwrap()
        .unwrap();
    assert!(msg.starts_with("Exported 38 records ("), "{}", msg);
    assert_eq!(n, 30 + 6 + 2);
    let files = export::read_zip(&std::fs::read(&zip_path).unwrap()).unwrap();
    let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
    assert_eq!(
        names,
        vec!["SBIN_NSE_1m.csv", "SBIN_NSE_5m.csv", "SBIN_NSE_W.csv"]
    );
    let one = String::from_utf8(files[0].1.clone()).unwrap();
    assert!(one.starts_with("date,time,open,high,low,close,volume,oi\n2024-01-01,09:15:00,100.0,101.5,99.25,100.5,1000,0\n"), "{}", one);
    let csv1 = dir.path().join("one.csv");
    std::fs::write(&csv1, &files[0].1).unwrap();
    assert_eq!(import_into(&db, &csv1, FileKind::Csv, "COPY1", "1m"), 30);
    let back = db
        .read(|c| db::stored(c, "COPY1", "NSE", "1m", None, None))
        .unwrap();
    assert_eq!(back, original);
    let five = String::from_utf8(files[1].1.clone()).unwrap();
    assert!(five.contains("\n2024-01-01,09:20:00,105.0,"), "{}", five);

    // Parquet written by COPY TO, read back.
    let pq = dir.path().join("x.parquet");
    let spec = ExportSpec {
        symbols: vec![("SBIN".into(), "NSE".into())],
        intervals: vec!["1m".into()],
        compression: "zstd".into(),
        ..Default::default()
    };
    let (msg, n) = db
        .read(|c| export::export_parquet(c, &pq, &spec))
        .unwrap()
        .unwrap();
    assert_eq!(n, 30);
    assert!(msg.starts_with("Exported 30 records ("));
    let cols: Vec<String> = db
        .read(|c| {
            let mut st = c.prepare(&format!(
                "DESCRIBE SELECT * FROM read_parquet('{}')",
                pq.display()
            ))?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    assert_eq!(
        cols,
        vec![
            "symbol",
            "exchange",
            "interval",
            "timestamp",
            "open",
            "high",
            "low",
            "close",
            "volume",
            "oi",
            "datetime"
        ]
    );
    assert_eq!(import_into(&db, &pq, FileKind::Parquet, "COPY2", "1m"), 30);
    assert_eq!(
        db.read(|c| db::stored(c, "COPY2", "NSE", "1m", None, None))
            .unwrap(),
        original
    );

    // Flat CSV and TXT: stored rows with symbol columns.
    let csv = dir.path().join("all.csv");
    let spec = ExportSpec {
        intervals: vec!["D".into()],
        ..Default::default()
    };
    let (msg, n) = db
        .read(|c| export::export_flat(c, &csv, &spec, ','))
        .unwrap()
        .unwrap();
    assert_eq!(msg, "Exported 14 records");
    assert_eq!(n, 14);
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("symbol,exchange,interval,date,time,open,high,low,close,volume,oi\nINFY,NSE,D,2024-01-01,00:00:00,10.0,12.0,9.0,11.0,100,5\n"), "{}", text);
    let txt = dir.path().join("all.txt");
    db.read(|c| export::export_flat(c, &txt, &spec, '\t'))
        .unwrap()
        .unwrap();
    assert!(std::fs::read_to_string(&txt)
        .unwrap()
        .starts_with("symbol\texchange\tinterval\tdate"));
    let none = dir.path().join("none.csv");
    let empty = ExportSpec {
        symbols: vec![("NOPE".into(), "NSE".into())],
        ..Default::default()
    };
    assert_eq!(
        db.read(|c| export::export_flat(c, &none, &empty, ','))
            .unwrap(),
        Err("No data matching the criteria".to_string())
    );
    assert!(!none.exists());
    assert_eq!(db.open_connections(), 0);
}

#[test]
fn sample_files_and_web_style_csvs_import() {
    let dir = tempfile::tempdir().unwrap();
    let db = HistorifyDb::new(&dir.path().join("h.duckdb")).unwrap();
    let s = dir.path().join("s.csv");
    std::fs::write(&s, export::sample_csv()).unwrap();
    assert_eq!(import_into(&db, &s, FileKind::Csv, "A", "D"), 5);
    let a = db
        .read(|c| db::stored(c, "A", "NSE", "D", None, None))
        .unwrap();
    assert_eq!(a[0].timestamp, OPEN_TS);
    assert_eq!(a[0].volume, 10000);
    let p = dir.path().join("s.parquet");
    std::fs::write(&p, export::sample_parquet(dir.path()).unwrap()).unwrap();
    assert_eq!(import_into(&db, &p, FileKind::Parquet, "B", "D"), 5);
    assert_eq!(
        db.read(|c| db::stored(c, "B", "NSE", "D", None, None))
            .unwrap(),
        a
    );

    // Epoch milliseconds, upper-case headers, a bad price row.
    let ms = dir.path().join("ms.csv");
    std::fs::write(
        &ms,
        "Timestamp, Open ,High,Low,Close,Volume\n1704080700000,1,2,0.5,1.5,10\n1704080760000,x,2,0.5,1.5,10\n",
    )
    .unwrap();
    let parsed = db
        .read(|c| import::read_file(c, &ms, FileKind::Csv))
        .unwrap()
        .unwrap();
    assert_eq!(parsed.bars.len(), 1);
    assert_eq!(parsed.dropped, 1);
    assert_eq!(parsed.bars[0].timestamp, OPEN_TS);

    let missing = dir.path().join("m.csv");
    std::fs::write(&missing, "date,open,high\n2024-01-01,1,2\n").unwrap();
    assert_eq!(
        db.read(|c| import::read_file(c, &missing, FileKind::Csv))
            .unwrap()
            .unwrap_err(),
        "Missing required columns: low, close, volume"
    );
    let no_time = dir.path().join("n.csv");
    std::fs::write(&no_time, "foo,open,high,low,close,volume\n1,1,2,0,1,1\n").unwrap();
    assert_eq!(
        db.read(|c| import::read_file(c, &no_time, FileKind::Csv))
            .unwrap()
            .unwrap_err(),
        "CSV must have 'timestamp', 'datetime', or 'date' column"
    );
    let empty = dir.path().join("e.csv");
    std::fs::write(&empty, "date,open,high,low,close,volume\n").unwrap();
    assert_eq!(
        db.read(|c| import::read_file(c, &empty, FileKind::Csv))
            .unwrap()
            .unwrap_err(),
        "CSV file is empty"
    );
    assert_eq!(db.open_connections(), 0);
}

// ----------------------------------------------------------------- routes

struct App {
    ctx: Arc<AppState>,
    mock: Arc<MockBroker>,
    cookie: String,
    csrf: String,
    _dir: tempfile::TempDir,
}

impl App {
    async fn new(connected: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let symbols = SymbolResolver::new();
        let mock = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
        *mock.history.lock() = Some(Ok(daily_bars(3)));
        let clock = ManualClock::new(ist(2026, 10, 7, 10, 0));
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(BrokerRegistry::with_symbols(
                    symbols,
                    vec![mock.clone() as Arc<dyn Broker>],
                )),
            },
        )
        .unwrap();
        ctx.load_symbol_cache(master());
        AuthService::setup(&ctx, "trader", "trader@example.com", "Secret@123").unwrap();
        if connected {
            BrokerAuthService::persist(
                &ctx,
                &BrokerSession {
                    broker_id: "zerodha".into(),
                    auth_token: "mock-access-token".into(),
                    feed_token: None,
                    user_id: "AB1234".into(),
                    user_name: None,
                    authenticated_at: ctx.now(),
                },
            )
            .unwrap();
        }
        let s = ctx.sessions.create(ctx.now());
        ctx.sessions
            .update(&s.id, |x| x.user = Some("trader".into()));
        App {
            ctx,
            mock,
            cookie: format!("session={}", s.id),
            csrf: s.csrf_token,
            _dir: dir,
        }
    }

    async fn raw(&self, mut req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        crate::with_host(&mut req, &self.ctx);
        let resp = openalgo_desktop_lib::server::app(self.ctx.clone())
            .oneshot(req)
            .await
            .unwrap();
        let (s, h) = (resp.status(), resp.headers().clone());
        let b = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (s, h, b)
    }

    fn req(
        &self,
        m: Method,
        path: &str,
        body: Option<Value>,
        auth: bool,
        csrf: bool,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(m)
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if auth {
            b = b.header(header::COOKIE, &self.cookie);
        }
        if csrf {
            b = b.header("x-csrftoken", &self.csrf);
        }
        match body {
            Some(v) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    }

    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (s, _, b) = self.raw(self.req(m, path, body, true, true)).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(body)).await
    }
}

fn concrete(path: &str) -> String {
    path.replace("{id}", "abc12345").replace("{format}", "csv")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_route_requires_the_user_and_writes_require_csrf() {
    let app = App::new(true).await;
    let specs = openalgo_desktop_lib::server::routes::historify::table();
    assert!(specs.len() >= 45);
    for spec in &specs {
        assert_eq!(
            spec.access,
            openalgo_desktop_lib::server::routes::Access::User,
            "{}",
            spec.path
        );
        let path = concrete(spec.path);
        let mutating = spec.method != Method::GET;
        // No session.
        let (s, _, b) = app
            .raw(app.req(
                spec.method.clone(),
                &path,
                mutating.then(|| json!({})),
                false,
                false,
            ))
            .await;
        if mutating {
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", spec.method, path);
        } else {
            assert_eq!(
                s,
                StatusCode::UNAUTHORIZED,
                "{} {}: {}",
                spec.method,
                path,
                String::from_utf8_lossy(&b)
            );
        }
        if mutating {
            // Signed in, but no CSRF token.
            let (s, _, b) = app
                .raw(app.req(spec.method.clone(), &path, Some(json!({})), true, false))
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", spec.method, path);
            let v: Value = serde_json::from_slice(&b).unwrap();
            assert_eq!(
                v["message"],
                "Your session has expired. Refresh the page and try again."
            );
        }
    }
    // Every page call in the contract is in the table.
    for p in [
        "/historify/api/watchlist",
        "/historify/api/watchlist/bulk",
        "/historify/api/watchlist/bulk/delete",
        "/historify/api/catalog",
        "/historify/api/catalog/metadata",
        "/historify/api/data",
        "/historify/api/export/bulk",
        "/historify/api/export/bulk/download",
        "/historify/api/intervals",
        "/historify/api/historify-intervals",
        "/historify/api/exchanges",
        "/historify/api/stats",
        "/historify/api/delete",
        "/historify/api/delete/bulk",
        "/historify/api/upload",
        "/historify/api/sample/{format}",
        "/historify/api/fno/underlyings",
        "/historify/api/fno/expiries",
        "/historify/api/fno/chain",
        "/historify/api/jobs",
        "/historify/api/jobs/{id}",
        "/historify/api/jobs/{id}/cancel",
        "/historify/api/jobs/{id}/pause",
        "/historify/api/jobs/{id}/resume",
        "/historify/api/jobs/{id}/retry",
        "/historify/api/schedules",
        "/historify/api/schedules/{id}",
        "/historify/api/schedules/{id}/enable",
        "/historify/api/schedules/{id}/disable",
        "/historify/api/schedules/{id}/pause",
        "/historify/api/schedules/{id}/resume",
        "/historify/api/schedules/{id}/trigger",
        "/historify/api/schedules/{id}/executions",
    ] {
        assert!(specs.iter().any(|s| s.path == p), "{}", p);
    }
    app.ctx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watchlist_catalog_data_and_utility_routes_answer_in_the_web_shapes() {
    let app = App::new(true).await;
    let (s, v) = app
        .post(
            "/historify/api/watchlist",
            json!({"symbol": "sbin", "exchange": "nse"}),
        )
        .await;
    assert_eq!(
        (s, v.clone()),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Added SBIN to watchlist"})
        )
    );
    let (_, v) = app
        .post(
            "/historify/api/watchlist",
            json!({"symbol": "SBIN", "exchange": "NSE"}),
        )
        .await;
    assert_eq!(v["message"], "SBIN already in watchlist");
    let (s, v) = app
        .post(
            "/historify/api/watchlist",
            json!({"symbol": "NOPE", "exchange": "NSE"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v["message"],
        "Symbol 'NOPE' not found in NSE master contract. Please check the symbol name."
    );
    let (s, v) = app
        .post(
            "/historify/api/watchlist",
            json!({"symbol": "SBIN", "exchange": "XYZ"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .starts_with("Invalid exchange. Supported: NSE, BSE, NFO"));
    let (s, _) = app.post("/historify/api/watchlist", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = app
        .post(
            "/historify/api/watchlist/bulk",
            json!({"symbols": [{"symbol": "INFY", "exchange": "NSE"}, {"symbol": "SBIN", "exchange": "NSE"},
                               {"symbol": "", "exchange": "NSE"}, {"symbol": "TCS", "exchange": "ZZZ"},
                               {"symbol": "NIFTY", "exchange": "NSE_INDEX"}]}),
        )
        .await;
    assert_eq!(v["added"], 2);
    assert_eq!(v["skipped"], 1);
    assert_eq!(v["total"], 5);
    assert_eq!(v["failed"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["failed"][0],
        json!({"symbol": "MISSING", "exchange": "NSE", "error": "Missing symbol or exchange"})
    );
    let (_, v) = app.get("/historify/api/watchlist").await;
    assert_eq!(v["count"], 3);
    let row = &v["data"][0];
    for k in ["id", "symbol", "exchange", "display_name", "added_at"] {
        assert!(row.get(k).is_some(), "{}", k);
    }
    assert!(row["added_at"].as_str().unwrap().ends_with(" GMT"));
    let (_, v) = app
        .post("/historify/api/watchlist/bulk/delete", json!({"symbols": [{"symbol": "INFY", "exchange": "NSE"}, {"symbol": "ZZ", "exchange": "NSE"}]}))
        .await;
    assert_eq!(
        v["message"],
        "Removed 1 symbol(s) from watchlist, 1 not found"
    );
    let (s, v) = app
        .post(
            "/historify/api/watchlist/bulk/delete",
            json!({"symbols": []}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("No symbols provided"))
    );
    let (s, v) = app
        .call(
            Method::DELETE,
            "/historify/api/watchlist",
            Some(json!({"symbol": "SBIN", "exchange": "NSE"})),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Removed SBIN from watchlist"))
    );

    // Utility routes.
    let (_, v) = app.get("/historify/api/historify-intervals").await;
    assert_eq!(v["storage_intervals"], json!(["1m", "D"]));
    assert_eq!(v["computed_intervals"], json!(["15m", "1h", "30m", "5m"]));
    assert_eq!(
        v["all_intervals"],
        json!(["15m", "1h", "1m", "30m", "5m", "D"])
    );
    let (_, v) = app.get("/historify/api/exchanges").await;
    assert_eq!(v["data"][0], "NSE");
    assert_eq!(v["data"].as_array().unwrap().len(), 13);
    let (_, v) = app.get("/historify/api/stats").await;
    for k in [
        "database_path",
        "database_size_mb",
        "total_records",
        "total_symbols",
        "watchlist_count",
    ] {
        assert!(v["data"].get(k).is_some(), "{}", k);
    }
    let (s, v) = app.get("/historify/api/intervals").await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(v["data"]["days"].is_array());

    // Data, catalog and deletes over seeded candles.
    seed(&app.ctx.historify.db);
    let (_, v) = app
        .get("/historify/api/data?symbol=sbin&exchange=NSE&interval=5m")
        .await;
    assert_eq!(v["count"], 6);
    assert_eq!(v["interval"], "5m");
    assert_eq!(
        v["data"][0],
        json!({"close": 104.5, "high": 105.5, "low": 99.25, "oi": 0, "open": 100.0, "timestamp": OPEN_TS, "volume": 5010})
    );
    let (_, v) = app.get("/historify/api/data?symbol=SBIN&exchange=NSE&interval=D&start_date=2024-01-03&end_date=2024-01-04").await;
    assert_eq!(v["count"], 3);
    let (_, v) = app
        .get("/historify/api/data?symbol=NONE&exchange=NSE")
        .await;
    assert_eq!(
        v,
        json!({"status": "success", "data": [], "count": 0, "message": "No data available"})
    );
    let (s, _) = app
        .get("/historify/api/data?symbol=SBIN&exchange=NSE&start_date=bad")
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = app.get("/historify/api/catalog").await;
    assert_eq!(v["count"], 3);
    let infy = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["symbol"] == "INFY")
        .unwrap()
        .clone();
    assert_eq!(infy["first_date"], "2024-01-01");
    assert_eq!(infy["last_date"], "2024-01-04");
    assert_eq!(infy["record_count"], 4);
    let (_, v) = app
        .post(
            "/historify/api/metadata/enrich",
            json!({"symbols": [{"symbol": "SBIN", "exchange": "NSE"}]}),
        )
        .await;
    assert_eq!(v["count"], 1);
    let (_, v) = app.get("/historify/api/catalog/metadata").await;
    let sb = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["symbol"] == "SBIN")
        .unwrap()
        .clone();
    assert_eq!(sb["name"], "SBIN");
    assert_eq!(sb["tick_size"], 0.05);
    let (_, v) = app
        .get("/historify/api/catalog/grouped?group_by=exchange")
        .await;
    assert_eq!(v["group_count"], 1);
    assert_eq!(v["data"]["NSE"].as_array().unwrap().len(), 3);
    let (_, v) = app
        .get("/historify/api/symbol-info?symbol=SBIN&exchange=NSE&interval=D")
        .await;
    assert_eq!(v["data"]["record_count"], 10);
    let (_, v) = app
        .call(
            Method::DELETE,
            "/historify/api/delete",
            Some(json!({"symbol": "SBIN", "exchange": "NSE", "interval": "1m"})),
        )
        .await;
    assert_eq!(v["message"], "Deleted SBIN:NSE:1m data");
    let (_, v) = app.post("/historify/api/delete/bulk", json!({"symbols": [{"symbol": "INFY", "exchange": "NSE"}, {"symbol": "TCS", "exchange": "NSE"}]})).await;
    assert_eq!(v["message"], "Deleted 1 symbol(s), 1 had no data");
    assert_eq!(v["deleted"], 1);

    // /api/v1/history source=db reads the same store.
    let key = ApiKeyService::current(&app.ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    let (s, v) = app
        .call(Method::POST, "/api/v1/history", Some(json!({"apikey": key, "symbol": "SBIN", "exchange": "NSE", "interval": "W", "start_date": "2024-01-01", "end_date": "2024-01-31", "source": "db"})))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["data"][0],
        json!({"close": 17.0, "high": 12.0, "low": 9.0, "oi": 5, "open": 10.0, "timestamp": 1_704_067_200, "volume": 700})
    );
    let (s, v) = app
        .call(Method::POST, "/api/v1/history", Some(json!({"apikey": key, "symbol": "INFY", "exchange": "NSE", "interval": "D", "start_date": "2024-01-01", "end_date": "2024-01-31", "source": "db"})))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["message"], "No data found for INFY:NSE interval D in local database. Download data first using Historify.");
    app.ctx.shutdown().await;
    assert_eq!(app.ctx.historify.db.open_connections(), 0);
}

fn multipart(fields: &[(&str, &str)], file: Option<(&str, &[u8])>) -> (String, Vec<u8>) {
    let boundary = "XHISTORIFYBOUNDARY";
    let mut body = Vec::new();
    if let Some((name, bytes)) = file {
        body.extend_from_slice(format!("--{}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n", boundary, name).as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    for (k, v) in fields {
        body.extend_from_slice(
            format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                boundary, k, v
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    (format!("multipart/form-data; boundary={}", boundary), body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_sample_and_export_download_routes() {
    let app = App::new(true).await;
    let upload = |fields: Vec<(&'static str, &'static str)>,
                  file: Option<(&'static str, Vec<u8>)>| {
        let (ct, body) = multipart(&fields, file.as_ref().map(|(n, b)| (*n, b.as_slice())));
        Request::builder()
            .method(Method::POST)
            .uri("/historify/api/upload")
            .header(header::ACCEPT, "application/json")
            .header(header::COOKIE, &app.cookie)
            .header("x-csrftoken", &app.csrf)
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap()
    };
    let fields = vec![("symbol", "sbin"), ("exchange", "NSE"), ("interval", "D")];
    let (s, _, b) = app
        .raw(upload(
            fields.clone(),
            Some(("data.csv", export::sample_csv().into_bytes())),
        ))
        .await;
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Imported 5 records", "symbol": "SBIN", "exchange": "NSE", "interval": "D", "records": 5})
    );
    let pq = export::sample_parquet(&app._dir.path().join("t")).unwrap();
    let (s, _, b) = app
        .raw(upload(
            vec![("symbol", "INFY"), ("exchange", "NSE"), ("interval", "D")],
            Some(("x.PARQUET", pq)),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    for (fields, file, msg) in [
        (fields.clone(), None, "No file provided"),
        (fields.clone(), Some(("data.xlsx", b"x".to_vec())), "File must be CSV or Parquet"),
        (fields.clone(), Some(("", b"x".to_vec())), "No file selected"),
        (vec![("symbol", "SBIN")], Some(("a.csv", b"x".to_vec())), "Symbol, exchange, and interval are required"),
        (vec![("symbol", "SBIN"), ("exchange", "NSE"), ("interval", "7m")], Some(("a.csv", export::sample_csv().into_bytes())), "Invalid interval. Supported: 10m, 10s, 15m, 15s, 1D, 1M, 1W, 1h, 1m, 1s, 20m, 2h, 2m, 30m, 30s, 3h, 3m, 45m, 4h, 5m, 5s, D, M, W"),
        (vec![("symbol", "SBIN"), ("exchange", "NSE"), ("interval", "D")], Some(("a.csv", b"date,open\n".to_vec())), "CSV file is empty"),
    ] {
        let (s, _, b) = app.raw(upload(fields, file)).await;
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!((s, v["message"].as_str().unwrap()), (StatusCode::BAD_REQUEST, msg));
    }
    // Nothing left behind in the work folder.
    let work = app._dir.path().join("historify_work");
    let left = std::fs::read_dir(&work).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0);

    // Samples.
    let (s, h, b) = app
        .raw(app.req(Method::GET, "/historify/api/sample/csv", None, true, false))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h[header::CONTENT_DISPOSITION],
        "attachment; filename=sample_ohlcv.csv"
    );
    assert_eq!(String::from_utf8(b).unwrap(), export::sample_csv());
    let (s, h, b) = app
        .raw(app.req(
            Method::GET,
            "/historify/api/sample/parquet",
            None,
            true,
            false,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(&b[..4], b"PAR1");
    let (s, v) = app.get("/historify/api/sample/xls").await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("Invalid format. Use csv or parquet")
        )
    );

    // Export then download once.
    let (s, v) = app
        .post(
            "/historify/api/export/bulk",
            json!({"format": "csv", "symbols": null, "intervals": ["D"], "compression": "zstd"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["record_count"], 10);
    assert_eq!(v["message"], "Exported 10 records");
    let name = v["filename"].as_str().unwrap().to_string();
    assert!(
        name.starts_with("historify_export_20261007_100000_") && name.ends_with(".csv"),
        "{}",
        name
    );
    let (s, h, b) = app
        .raw(app.req(
            Method::GET,
            "/historify/api/export/bulk/download",
            None,
            true,
            false,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "text/csv");
    assert_eq!(
        h[header::CONTENT_DISPOSITION].to_str().unwrap(),
        format!("attachment; filename={}", name)
    );
    assert_eq!(String::from_utf8(b).unwrap().lines().count(), 11);
    let (s, v) = app.get("/historify/api/export/bulk/download").await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::NOT_FOUND, json!("Export file not found"))
    );
    // Multiple intervals force a ZIP; a computed interval in CSV too.
    let (_, v) = app.post("/historify/api/export/bulk", json!({"format": "csv", "symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "intervals": ["D", "W"]})).await;
    let zname = v["filename"].as_str().unwrap().to_string();
    assert!(
        zname.starts_with("historify_SBIN_") && zname.ends_with(".zip"),
        "{}",
        zname
    );
    let (_, h, b) = app
        .raw(app.req(
            Method::GET,
            "/historify/api/export/bulk/download",
            None,
            true,
            false,
        ))
        .await;
    assert_eq!(h[header::CONTENT_TYPE], "application/zip");
    assert_eq!(export::read_zip(&b).unwrap().len(), 2);
    let (_, v) = app
        .post(
            "/historify/api/export/bulk",
            json!({"format": "parquet", "intervals": ["D"], "compression": "snappy"}),
        )
        .await;
    assert!(v["filename"].as_str().unwrap().ends_with(".parquet"));
    let (s, v) = app
        .post("/historify/api/export/bulk", json!({"intervals": ["MO"]}))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("Invalid intervals: ['MO']"))
    );
    let (s, v) = app
        .post("/historify/api/export/bulk", json!({"intervals": []}))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("At least one interval must be specified")
        )
    );
    let (s, v) = app
        .post("/historify/api/export/bulk", json!({"intervals": "D"}))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("intervals must be an array"))
    );
    let (s, v) = app
        .post(
            "/historify/api/export/bulk",
            json!({"symbols": [{"symbol": "NONE", "exchange": "NSE"}], "intervals": ["D"]}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("No data matching the criteria")
        )
    );
    let (_, v) = app
        .post("/historify/api/export/preview", json!({"interval": "D"}))
        .await;
    assert_eq!(v["data"]["total_records"], 10);
    app.ctx.shutdown().await;
    // Pending export files are removed at shutdown.
    assert_eq!(
        std::fs::read_dir(&work)
            .map(|d| d
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "parquet"))
                .count())
            .unwrap_or(0),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fno_routes_follow_the_master() {
    let app = App::new(true).await;
    let (_, v) = app.get("/historify/api/fno/underlyings?exchange=NFO").await;
    assert_eq!(
        v,
        json!({"status": "success", "data": ["NIFTY"], "count": 1, "exchange": "NFO"})
    );
    let (_, v) = app.get("/historify/api/fno/underlyings").await;
    assert_eq!(v["exchange"], "ALL");
    let (s, v) = app.get("/historify/api/fno/underlyings?exchange=NSE").await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("Invalid FNO exchange. Supported: BCD, BFO, CDS, CRYPTO, MCX, NCDEX, NCO, NFO")
        )
    );
    let (_, v) = app
        .get("/historify/api/fno/expiries?underlying=nifty&exchange=NFO")
        .await;
    assert_eq!(v["data"], json!(["28-OCT-26", "25-NOV-26"]));
    assert_eq!(v["underlying"], "NIFTY");
    let (s, v) = app.get("/historify/api/fno/expiries").await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("Underlying is required"))
    );
    let (_, v) = app.get("/historify/api/fno/chain?underlying=NIFTY&expiry=28-OCT-26&instrumenttype=CE&strike_min=25050").await;
    assert_eq!(v["count"], 1);
    assert_eq!(v["data"][0]["symbol"], "NIFTY28OCT2625100CE");
    for k in [
        "brsymbol",
        "name",
        "token",
        "lotsize",
        "instrumenttype",
        "tick_size",
        "freeze_qty",
    ] {
        assert!(v["data"][0].get(k).is_some(), "{}", k);
    }
    let (_, v) = app
        .get("/historify/api/fno/chain?underlying=NIFTY&limit=2")
        .await;
    assert_eq!(v["count"], 2);
    let (_, v) = app.get("/historify/api/fno/futures?underlying=NIFTY").await;
    assert_eq!(v["data"][0]["symbol"], "NIFTY28OCT26FUT");
    let (_, v) = app
        .get("/historify/api/fno/options?underlying=NIFTY&expiry=28-OCT-26")
        .await;
    assert_eq!(
        (v["ce_count"].clone(), v["pe_count"].clone()),
        (json!(2), json!(1))
    );
    app.ctx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_and_schedule_routes_validate_and_answer_like_the_web() {
    let app = App::new(true).await;
    let (s, v) = app
        .post("/historify/api/jobs", json!({"symbols": []}))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("No symbols provided"))
    );
    let (s, v) = app
        .post(
            "/historify/api/jobs",
            json!({"symbols": [{"symbol": "SBIN"}]}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    let (s, v) = app
        .post(
            "/historify/api/jobs",
            json!({"symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "start_date": "2024-01-01"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    let (s, v) = app
        .post("/historify/api/jobs", json!({"job_type": "watchlist", "symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "interval": "D", "start_date": "2024-01-01", "end_date": "2024-01-05", "incremental": true}))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["message"], "Job started with 1 symbols");
    assert_eq!(v["incremental"], true);
    let id = v["job_id"].as_str().unwrap().to_string();
    let (_, v) = app.get("/historify/api/jobs?limit=50").await;
    assert_eq!(v["count"], 1);
    for k in [
        "id",
        "job_type",
        "status",
        "total_symbols",
        "completed_symbols",
        "failed_symbols",
        "interval",
        "start_date",
        "end_date",
        "created_at",
        "started_at",
        "completed_at",
    ] {
        assert!(v["data"][0].get(k).is_some(), "{}", k);
    }
    assert!(v["data"][0].get("config").is_none());
    let (s, v) = app.get(&format!("/historify/api/jobs/{}", id)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["items"].is_array() && v["job"]["config"].is_string());
    let (s, v) = app.get("/historify/api/jobs/zzzzzzzz").await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::NOT_FOUND, json!("Job not found"))
    );
    for action in ["cancel", "pause", "resume", "retry"] {
        let (s, v) = app
            .post(
                &format!("/historify/api/jobs/zzzzzzzz/{}", action),
                json!({}),
            )
            .await;
        assert_eq!(
            (s, v["message"].clone()),
            (StatusCode::NOT_FOUND, json!("Job not found")),
            "{}",
            action
        );
    }
    // The job completes after the web's pacing (1-3 s after the symbol).
    for _ in 0..800 {
        if app.ctx.historify.jobs.status(&id).await.body["job"]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (_, v) = app.get(&format!("/historify/api/jobs/{}", id)).await;
    assert_eq!(v["job"]["status"], "completed");
    let (s, v) = app
        .call(Method::DELETE, &format!("/historify/api/jobs/{}", id), None)
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!(format!("Job {} deleted", id)))
    );
    assert!(history_calls(&app.mock, "SBIN") >= 1);

    // Schedules.
    for (body, msg) in [
        (
            json!({"schedule_type": "daily"}),
            "Schedule name is required",
        ),
        (
            json!({"name": "x", "schedule_type": "weekly"}),
            "Invalid schedule type. Must be \"interval\" or \"daily\"",
        ),
        (
            json!({"name": "x", "schedule_type": "daily", "data_interval": "5m"}),
            "Invalid data interval. Must be \"1m\" or \"D\"",
        ),
        (
            json!({"name": "x", "schedule_type": "interval", "interval_value": 0}),
            "Invalid interval value",
        ),
        (
            json!({"name": "x", "schedule_type": "interval", "interval_value": 5, "interval_unit": "days"}),
            "Invalid interval unit. Must be \"minutes\" or \"hours\"",
        ),
        (
            json!({"name": "x", "schedule_type": "daily", "time_of_day": "25:00"}),
            "Invalid time format. Use HH:MM (e.g., 09:15)",
        ),
        (
            json!({"name": "x", "schedule_type": "daily", "lookback_days": 400}),
            "lookback_days must be between 1 and 365",
        ),
    ] {
        let (s, v) = app.post("/historify/api/schedules", body.clone()).await;
        assert_eq!(
            (s, v["message"].as_str().unwrap()),
            (StatusCode::BAD_REQUEST, msg),
            "{}",
            body
        );
    }
    let (s, v) = app.post("/historify/api/schedules", json!({"name": " Close ", "schedule_type": "daily", "time_of_day": "15:45", "data_interval": "D", "lookback_days": 2})).await;
    assert_eq!(s, StatusCode::CREATED, "{}", v);
    assert_eq!(v["message"], "Schedule 'Close' created successfully");
    let sid = v["schedule_id"].as_str().unwrap().to_string();
    let (_, v) = app.get("/historify/api/schedules").await;
    assert_eq!(v["count"], 1);
    let row = &v["data"][0];
    assert_eq!(row["next_run_at"], "2026-10-07T15:45:00+05:30");
    assert_eq!(row["is_enabled"], true);
    assert_eq!(row["is_paused"], false);
    assert_eq!(row["status"], "idle");
    assert_eq!(row["interval_unit"], "minutes");
    for k in [
        "description",
        "interval_value",
        "last_run_at",
        "last_run_status",
    ] {
        assert!(row.get(k).is_some_and(Value::is_null), "{}", k);
    }
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/historify/api/schedules/{}", sid),
            Some(json!({"time_of_day": "16:00", "name": "Later"})),
        )
        .await;
    assert_eq!(
        (s, v.clone()),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Schedule updated successfully"})
        )
    );
    let (_, v) = app.get(&format!("/historify/api/schedules/{}", sid)).await;
    assert_eq!(v["data"]["next_run_at"], "2026-10-07T16:00:00+05:30");
    assert_eq!(v["data"]["name"], "Later");
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/historify/api/schedules/{}", sid),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("No fields to update"))
    );
    let (s, _) = app
        .call(
            Method::PUT,
            "/historify/api/schedules/zzzzzzzz",
            Some(json!({"name": "a"})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app.get("/historify/api/schedules/zzzzzzzz").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    for (action, msg) in [
        ("pause", "Schedule paused"),
        ("resume", "Schedule resumed"),
        ("disable", "Schedule disabled"),
        ("enable", "Schedule enabled"),
    ] {
        let (s, v) = app
            .post(
                &format!("/historify/api/schedules/{}/{}", sid, action),
                json!({}),
            )
            .await;
        assert_eq!(
            (s, v),
            (StatusCode::OK, json!({"status": "success", "message": msg}))
        );
    }
    let (s, v) = app
        .post(
            &format!("/historify/api/schedules/{}/trigger", sid),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Schedule triggered"))
    );
    let (s, v) = app
        .post("/historify/api/schedules/zzzzzzzz/trigger", json!({}))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("Schedule not found"))
    );
    let (_, v) = app
        .get(&format!(
            "/historify/api/schedules/{}/executions?limit=500",
            sid
        ))
        .await;
    assert_eq!(v["status"], "success");
    assert!(v["count"].is_number());
    let (s, v) = app
        .call(
            Method::DELETE,
            &format!("/historify/api/schedules/{}", sid),
            None,
        )
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::OK, json!("Schedule deleted successfully"))
    );
    app.ctx.shutdown().await;
    assert_eq!(app.ctx.historify.jobs.task_count(), 0);
    assert_eq!(app.ctx.historify.db.open_connections(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downloads_need_a_connected_broker() {
    let app = App::new(false).await;
    let (s, v) = app
        .post("/historify/api/jobs", json!({"symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "start_date": "2024-01-01", "end_date": "2024-01-02"}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v["message"],
        openalgo_desktop_lib::historify::source::BROKER_NOT_CONNECTED
    );
    let (s, v) = app.get("/historify/api/intervals").await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("Connect your broker to see the timeframes it offers.")
        )
    );
    app.ctx.shutdown().await;
}
