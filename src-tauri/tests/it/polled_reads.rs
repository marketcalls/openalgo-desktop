//! The reads the trading pages repeat while they poll, over a long history
//! (follow-up of "Async-worker DB reads"): the strategy module's books,
//! orders and `/api/v1/strategy` history, an OpenScript deployment's books
//! and status, the Action Center, the analyzer status and the bots'
//! statistics. None holds the async runtime that carries the feed and the
//! risk owners, and the reads a page asks for together are read once.
//!
//! Every history here is kept forever, as on the web (maintainer, log
//! retention), so each grows with use: a year of one strategy, one
//! deployment, the Action Center and the bots is seeded in one statement.

use crate::api_v1_support::H;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use openalgo_desktop_lib::db::sqlite::checkouts;
use openalgo_desktop_lib::security::hashing;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tower::ServiceExt;

type App = tower_http::normalize_path::NormalizePath<axum::Router>;
type Reply = (StatusCode, Vec<u8>);

const USER: &str = "trader";
const STRATEGY_RUNS: i64 = 250;
const STRATEGY_ORDERS: i64 = 25_000;
const LEDGER_ORDERS: i64 = 40_000;
const PENDING_ORDERS: i64 = 10_000;
const BOT_COMMANDS: i64 = 200_000;

/// The signed-in browser's cookie.
fn cookie(h: &H) -> String {
    signed_in(h).0
}

/// The signed-in browser's cookie and CSRF token.
fn signed_in(h: &H) -> (String, String) {
    let s = h.ctx.sessions.create(h.ctx.now());
    h.ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
    (format!("session={}", s.id), s.csrf_token)
}

/// A page's GET, as the signed-in browser.
fn page(path: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(header::ACCEPT, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

/// An `/api/v1` POST with the API key.
fn api(h: &H, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(h.with_key(body).to_string()))
        .unwrap()
}

/// `req` through the whole router (built once, before anything is timed).
async fn send(h: &H, app: &App, mut req: Request<Body>) -> Reply {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
    crate::with_host(&mut req, &h.ctx);
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec())
}

/// The app, once the runtime has gone idle after opening it. The
/// background tasks it starts (the scalping stops, the order reconciler)
/// make their first pass then. That is the app starting, not the reads
/// timed here: run at the same time as the requests, it waited on the
/// runtime for a connection while the pool was still opening them (30-63
/// ms on an M1, before those passes read off the runtime).
async fn started(runtime: &crate::IdleLog) -> H {
    let h = H::new().await;
    runtime.until_idle().await;
    h
}

/// Every request at once; the replies.
async fn all_at_once(h: &H, app: &App, reqs: Vec<Request<Body>>) -> Vec<Reply> {
    futures_util::future::join_all(reqs.into_iter().map(|r| send(h, app, r))).await
}

/// What requests sent at once cost the runtime of [`crate::on_watched_runtime`].
struct Held {
    /// The longest its thread ran without going idle meanwhile
    /// ([`crate::IdleLog::longest_hold`]).
    at_once: Duration,
    /// Each request's polls in all ([`crate::runtime_busy`]).
    each: Vec<Duration>,
    /// Argon2 runs on the runtime thread meanwhile. A key's first check
    /// (the `/api/v1` requests here) verifies it on the blocking pool; on
    /// the runtime that one run held it for 30-45 ms in a debug build and
    /// about 200 ms on a loaded macOS runner.
    argon2: u32,
    /// Pooled database connections taken on the runtime thread meanwhile,
    /// and the time it waited for them.
    pooled: (u32, Duration),
}

/// [`all_at_once`] on the watched runtime; the replies and what they held
/// it for. A 10 ms timer beside the requests ([`crate::runtime_stall`])
/// also counts the time a loaded runner leaves the idle runtime thread
/// waiting to run again, which no request can change: the bot statistics'
/// timer once ran 314 ms late on a macOS runner (run 39737924664).
async fn held_at_once(
    runtime: &crate::IdleLog,
    h: &H,
    app: &App,
    reqs: Vec<Request<Body>>,
) -> (Vec<Reply>, Held) {
    let argon2 = hashing::argon2_runs();
    let pooled = checkouts();
    let each = reqs
        .into_iter()
        .map(|r| crate::runtime_busy(send(h, app, r)));
    let (replies, at_once) = runtime
        .longest_hold(futures_util::future::join_all(each))
        .await;
    let (replies, each) = replies.into_iter().unzip();
    let argon2 = hashing::argon2_runs() - argon2;
    let pooled = (checkouts().0 - pooled.0, checkouts().1 - pooled.1);
    (
        replies,
        Held {
            at_once,
            each,
            argon2,
            pooled,
        },
    )
}

fn assert_runtime_free(what: &str, held: &Held) {
    assert_eq!(
        held.argon2, 0,
        "{}: a key was verified on the runtime",
        what
    );
    let busy: Duration = held.each.iter().sum();
    eprintln!(
        "{}: the runtime was held for {:?} at most, busy {:?} in all (each request {:?}); \
         pooled connections taken on it {} (waited {:?})",
        what, held.at_once, busy, held.each, held.pooled.0, held.pooled.1
    );
    // A pooled connection taken on the runtime holds it for as long as the
    // pool has none free, which the other requests' reads decide: the
    // strategy pages' lookup waited 114 and 173 ms on a macOS runner.
    assert_eq!(
        held.pooled.0, 0,
        "{}: pooled database connections were taken on the runtime (waited {:?})",
        what, held.pooled.1
    );
    assert!(
        held.at_once < Duration::from_millis(200),
        "{}: the runtime was held for {:?} at once (each request {:?})",
        what,
        held.at_once,
        held.each
    );
    assert!(
        busy < Duration::from_secs(1),
        "{}: the reads kept the runtime busy for {:?} in all (each request {:?})",
        what,
        busy,
        held.each
    );
}

fn json_of(reply: &Reply) -> Value {
    serde_json::from_slice(&reply.1).unwrap_or(Value::Null)
}

// ------------------------------------------------------------ the seeds

/// A strategy of the trader's with `STRATEGY_ORDERS` orders over
/// `STRATEGY_RUNS` finished live runs; its id.
fn seed_strategy(h: &H) -> i64 {
    let config = json!({
        "name": "Long history",
        "underlying": "SBIN",
        "underlying_exchange": "NSE",
        "universe_tab": "stocks_fno",
        "product": "CNC",
        "strategy_type": "positional",
        "legs": [{"id": 1, "segment": "cash", "position": "B", "lots": 1, "sl_pts": 10.0}],
    });
    let (row, _) = h.ctx.strategy.store.create_strategy(USER, &config).unwrap();
    let c = h.ctx.sqlite.conn().unwrap();
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2)
         INSERT INTO sm_strategy_run (strategy_id, mode, broker, started_at, stopped_at,
             stop_reason, pnl_realized)
         SELECT ?1, 'live', 'zerodha', datetime('2026-10-03 04:00:00', '-' || i || ' days'),
             datetime('2026-10-03 04:00:00', '-' || i || ' days', '+6 hours'), 'manual', (i % 7) * 10.0
         FROM n",
        rusqlite::params![row.id, STRATEGY_RUNS],
    )
    .unwrap();
    let first: i64 = c
        .query_row(
            "SELECT MIN(id) FROM sm_strategy_run WHERE strategy_id = ?1",
            [row.id],
            |r| r.get(0),
        )
        .unwrap();
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO sm_strategy_order (run_id, leg_id, kind, broker_order_id, symbol,
             exchange, action, qty, product, pricetype, status, placed_at, filled_at,
             avg_fill_price, filled_qty)
         SELECT ?2 + i % ?3, 1, CASE i % 2 WHEN 0 THEN 'entry' ELSE 'exit' END, 'SM' || i,
             'SBIN', 'NSE', CASE i % 2 WHEN 0 THEN 'BUY' ELSE 'SELL' END, 1, 'CNC', 'MARKET',
             'complete', datetime('2026-10-03 04:00:00', '-' || i || ' minutes'),
             datetime('2026-10-03 04:00:00', '-' || i || ' minutes'), 800.0, 1
         FROM n",
        rusqlite::params![STRATEGY_ORDERS, first, STRATEGY_RUNS],
    )
    .unwrap();
    row.id
}

/// An OpenScript deployment (sandbox) with `LEDGER_ORDERS` orders; its id.
fn seed_deployment(h: &H) -> String {
    let id = openalgo_desktop_lib::trading::runner::store::deployment_id(
        "long.py", "SBIN", "NSE", "5m", "a1b2c3",
    );
    let c = h.ctx.sqlite.conn().unwrap();
    c.execute(
        "INSERT INTO openscript_deployments (deployment, script, symbol, exchange, interval,
             product, user_id, mode, updated_at)
         VALUES (?1, 'long.py', 'SBIN', 'NSE', '5m', 'MIS', ?2, 'sandbox',
             '2026-10-03 09:00:00 IST')",
        rusqlite::params![id, USER],
    )
    .unwrap();
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2)
         INSERT INTO openscript_orders (deployment, mode, tag, symbol, exchange, action,
             quantity, pricetype, product, orderid, status, filled_quantity, average_price,
             is_exit, placed_at, updated_at, broker, account)
         SELECT ?1, 'sandbox', ?1, 'SBIN', 'NSE', CASE i % 2 WHEN 0 THEN 'BUY' ELSE 'SELL' END,
             1, 'MARKET', 'MIS', 'OS' || i, 'complete', 1, 800.0, i % 2,
             datetime('2026-10-03 04:00:00', '-' || i || ' minutes'),
             datetime('2026-10-03 04:00:00', '-' || i || ' minutes'), 'sandbox', ''
         FROM n",
        rusqlite::params![id, LEDGER_ORDERS],
    )
    .unwrap();
    id
}

/// The trader's Action Center orders, every tenth still pending.
fn seed_pending(h: &H) {
    h.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO pending_orders (user_id, api_type, order_data, created_at,
                 created_at_ist, status)
             SELECT ?2, 'placeorder',
                 json_object('strategy', 'long', 'symbol', 'SBIN', 'exchange', 'NSE',
                     'action', 'BUY', 'quantity', '1', 'pricetype', 'MARKET', 'product', 'MIS'),
                 strftime('%Y-%m-%dT%H:%M:%SZ', '2026-10-03 04:00:00', '-' || i || ' minutes'),
                 strftime('%d-%m-%Y %H:%M:%S', '2026-10-03 09:30:00', '-' || i || ' minutes'),
                 CASE WHEN i % 10 = 0 THEN 'pending' ELSE 'approved' END
             FROM n",
            rusqlite::params![PENDING_ORDERS, USER],
        )
        .unwrap();
}

/// Both bots' command logs, `BOT_COMMANDS` each over the last five days.
fn seed_commands(h: &H) {
    let c = h.ctx.sqlite.conn().unwrap();
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO command_logs (telegram_id, command, chat_id, parameters, executed_at)
         SELECT 1000 + i % 50, CASE i % 3 WHEN 0 THEN 'orderbook' WHEN 1 THEN 'positions'
                 ELSE 'funds' END, 1, NULL,
             datetime('2026-10-03 04:00:00', '-' || (i * 2) || ' seconds')
         FROM n",
        [BOT_COMMANDS],
    )
    .unwrap();
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO whatsapp_command_logs (whatsapp_jid, command, parameters, executed_at)
         SELECT (9100000000 + i % 50) || '@s.whatsapp.net', CASE i % 3 WHEN 0 THEN 'orderbook'
                 WHEN 1 THEN 'positions' ELSE 'funds' END, NULL,
             datetime('2026-10-03 04:00:00', '-' || (i * 2) || ' seconds')
         FROM n",
        [BOT_COMMANDS],
    )
    .unwrap();
}

// ------------------------------------------------- reads counted, per flight

/// How many times each shared read ran, from its debug line
/// (`KeyedFlight`); one process per test (`isolated!`).
#[derive(Default)]
struct Reads(Mutex<Vec<String>>);

impl Reads {
    fn of(&self, what: &str) -> usize {
        self.0.lock().iter().filter(|m| m.contains(what)).count()
    }
}

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        }
    }
}

struct Count(Arc<Reads>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut m = Message(String::new());
        event.record(&mut m);
        self.0 .0.lock().push(m.0);
    }
}

fn count_reads() -> Arc<Reads> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;
    static READS: OnceLock<Arc<Reads>> = OnceLock::new();
    READS
        .get_or_init(|| {
            let reads = Arc::new(Reads::default());
            let filter = tracing_subscriber::filter::Targets::new().with_target(
                "openalgo_desktop_lib::services::report_flight",
                tracing::Level::DEBUG,
            );
            let subscriber =
                tracing_subscriber::registry().with(Count(reads.clone()).with_filter(filter));
            let _ = tracing::subscriber::set_global_default(subscriber);
            reads
        })
        .clone()
}

// -------------------------------------------------------- the strategy

fn strategy_pages(h: &H, cookie: &str, sid: i64) -> Vec<Request<Body>> {
    let s = format!("/strategy/api/strategies/{}", sid);
    let mut all = Vec::new();
    for _ in 0..2 {
        for book in ["orderbook", "tradebook", "positions", "orders"] {
            all.push(page(&format!("{}/{}", s, book), cookie));
        }
        all.push(api(
            h,
            "/api/v1/strategy/orders",
            json!({"strategy_id": sid}),
        ));
    }
    all
}

/// The Detail page's three books (polled every 5 s), its orders and an SDK
/// client's `/api/v1/strategy/orders`, two of each at once over a year of
/// one strategy. Each read every order the strategy ever placed on the
/// async worker.
#[test]
fn strategy_pages_over_a_long_history_do_not_hold_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let h = started(&runtime).await;
        let sid = seed_strategy(&h);
        let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
        let (replies, held) = held_at_once(&runtime, &h, &app, strategy_pages(&h, &c, sid)).await;
        for (i, r) in replies.iter().enumerate() {
            assert_eq!(
                r.0,
                StatusCode::OK,
                "{}: {}",
                i,
                String::from_utf8_lossy(&r.1)
            );
        }
        let orders = json_of(&replies[3]);
        assert_eq!(
            orders["data"].as_array().unwrap().len() as i64,
            STRATEGY_ORDERS
        );
        assert_eq!(orders["data"][0]["mode"], "live", "the Detail page's shape");
        let api_orders = json_of(&replies[4]);
        assert_eq!(
            api_orders["data"].as_array().unwrap().len() as i64,
            STRATEGY_ORDERS
        );
        assert!(
            api_orders["data"][0].get("mode").is_none(),
            "the web's shape"
        );
        assert_runtime_free("strategy pages", &held);
        h.shutdown().await;
    })
}

/// The three books the Detail page polls together read the strategy's
/// order history once between them.
#[tokio::test]
async fn the_books_polled_together_read_the_strategy_history_once() {
    crate::isolated!(the_books_polled_together_read_the_strategy_history_once);
    let reads = count_reads();
    let h = H::new().await;
    let sid = seed_strategy(&h);
    let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
    let s = format!("/strategy/api/strategies/{}", sid);
    let books = ["orderbook", "tradebook", "positions"]
        .iter()
        .map(|b| page(&format!("{}/{}", s, b), &c))
        .collect();
    let replies = all_at_once(&h, &app, books).await;
    assert!(replies.iter().all(|r| r.0 == StatusCode::OK));
    assert_eq!(reads.of("a strategy's order history"), 1);
    h.shutdown().await;
}

// ------------------------------------------------------ the deployment

fn deployment_pages(cookie: &str, id: &str) -> Vec<Request<Body>> {
    let mut all = Vec::new();
    for _ in 0..2 {
        for what in ["positions", "orderbook", "tradebook", "status"] {
            all.push(page(&format!("/openscript/runner/{}/{}", what, id), cookie));
        }
    }
    all
}

/// A running deployment's positions (polled every 15 s by the Strategies
/// panel), its other books and its status, two of each at once over a
/// year's ledger. Each read the whole ledger on the async worker.
#[test]
fn deployment_books_over_a_long_ledger_do_not_hold_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let h = started(&runtime).await;
        let id = seed_deployment(&h);
        let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
        let (replies, held) = held_at_once(&runtime, &h, &app, deployment_pages(&c, &id)).await;
        for (i, r) in replies.iter().enumerate() {
            // The books answer what the sandbox says (502 when its engine is
            // not running); the ledger was read either way.
            assert!(
                r.0 == StatusCode::OK || r.0 == StatusCode::BAD_GATEWAY,
                "{}: {} {}",
                i,
                r.0,
                String::from_utf8_lossy(&r.1)
            );
        }
        assert_eq!(json_of(&replies[3])["file"], id.as_str());
        assert_runtime_free("deployment books", &held);
        h.shutdown().await;
    })
}

/// The books asked for together read the deployment's own orders once
/// between them.
#[tokio::test]
async fn the_books_asked_together_read_the_deployments_orders_once() {
    crate::isolated!(the_books_asked_together_read_the_deployments_orders_once);
    let reads = count_reads();
    let h = H::new().await;
    let id = seed_deployment(&h);
    let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
    let books = ["positions", "orderbook", "tradebook"]
        .iter()
        .map(|b| page(&format!("/openscript/runner/{}/{}", b, id), &c))
        .collect();
    all_at_once(&h, &app, books).await;
    assert_eq!(reads.of("a deployment's own orders"), 1);
    h.shutdown().await;
}

// ------------------------------------------------- the Action Center

/// The Action Center reloads on every order event: four reloads and two of
/// the full list at once over a year of orders. Each read and parsed every
/// order twice on the async worker.
#[test]
fn the_action_center_over_many_orders_does_not_hold_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let h = started(&runtime).await;
        seed_pending(&h);
        let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
        let mut reqs: Vec<_> = (0..4)
            .map(|_| page("/action-center/api/data", &c))
            .collect();
        reqs.push(page("/action-center/api/data?status=all", &c));
        reqs.push(page("/action-center/api/data?status=all", &c));
        let (replies, held) = held_at_once(&runtime, &h, &app, reqs).await;
        assert!(replies.iter().all(|r| r.0 == StatusCode::OK));
        let pending = json_of(&replies[0]);
        assert_eq!(
            pending["data"]["orders"].as_array().unwrap().len() as i64,
            PENDING_ORDERS / 10
        );
        assert_eq!(
            pending["data"]["statistics"]["total_approved"],
            PENDING_ORDERS * 9 / 10
        );
        let all = json_of(&replies[4]);
        assert_eq!(
            all["data"]["orders"].as_array().unwrap().len() as i64,
            PENDING_ORDERS
        );
        assert_runtime_free("the Action Center", &held);
        h.shutdown().await;
    })
}

/// Reloads that arrive together share a read: the first reads, and the
/// three that asked while it ran share the next one (each answer is read
/// after its reload asked, so one that follows an approval shows it).
#[tokio::test]
async fn action_center_reloads_together_share_a_read() {
    crate::isolated!(action_center_reloads_together_share_a_read);
    let reads = count_reads();
    let h = H::new().await;
    seed_pending(&h);
    let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
    let reqs = (0..4)
        .map(|_| page("/action-center/api/data", &c))
        .collect();
    let replies = all_at_once(&h, &app, reqs).await;
    assert!(replies.iter().all(|r| r.0 == StatusCode::OK));
    assert!(replies.iter().all(|r| r.1 == replies[0].1), "one answer");
    assert_eq!(reads.of("the Action Center"), 2);
    h.shutdown().await;
}

// ------------------------------------------- the analyzer and the bots

/// `/api/v1/analyzer` (also the MCP `analyzer_status` tool) counts the whole
/// analyzer log, kept forever as on the web. While the logs database is
/// busy (its four connections held for 1.5 s, as long reports do), the
/// count waits off the runtime; it waited on the async worker before.
#[test]
fn analyzer_status_waits_for_the_logs_database_off_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let h = started(&runtime).await;
        let app = openalgo_desktop_lib::server::app(h.ctx.clone());
        let (keep, release) = std::sync::mpsc::channel::<()>();
        let (holding, ready) = std::sync::mpsc::channel::<()>();
        let logs = h.ctx.logs.clone();
        let holder = std::thread::spawn(move || {
            let conns: Vec<_> = (0..4).map(|_| logs.conn().unwrap()).collect();
            holding.send(()).unwrap();
            let _ = release.recv_timeout(Duration::from_millis(1500));
            drop(conns);
        });
        ready.recv().unwrap();
        let reqs = (0..2)
            .map(|_| api(&h, "/api/v1/analyzer", json!({})))
            .collect();
        let (replies, held) = held_at_once(&runtime, &h, &app, reqs).await;
        drop(keep);
        holder.join().unwrap();
        for r in &replies {
            let v = json_of(r);
            assert_eq!(r.0, StatusCode::OK, "{}", v);
            assert_eq!(v["data"]["total_logs"], 0);
        }
        assert_runtime_free("analyzer status", &held);
        h.shutdown().await;
    })
}

/// The Telegram page (its index, users and analytics), an SDK client's
/// `/api/v1/telegram/stats` and the WhatsApp page's statistics over a year,
/// over both bots' command logs (kept forever as on the web). Each read
/// its aggregates on the async worker.
#[test]
fn bot_statistics_over_a_long_command_log_do_not_hold_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let h = started(&runtime).await;
        seed_commands(&h);
        let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
        let stats_url = format!("/api/v1/telegram/stats?days=30&apikey={}", h.key);
        let reqs = vec![
            page("/telegram/api/index", &c),
            page("/telegram/api/users", &c),
            page("/telegram/api/analytics", &c),
            page(&stats_url, &c),
            page("/whatsapp/stats?days=365", &c),
            page("/whatsapp/stats?days=7", &c),
        ];
        let (replies, held) = held_at_once(&runtime, &h, &app, reqs).await;
        for (i, r) in replies.iter().enumerate() {
            assert_eq!(
                r.0,
                StatusCode::OK,
                "{}: {}",
                i,
                String::from_utf8_lossy(&r.1)
            );
        }
        assert_eq!(json_of(&replies[0])["data"]["total_commands"], BOT_COMMANDS);
        assert_eq!(json_of(&replies[4])["data"]["total_commands"], BOT_COMMANDS);
        assert_runtime_free("bot statistics", &held);
        h.shutdown().await;
    })
}

/// Migration 095: the WhatsApp statistics read only the window they count,
/// through an index on `executed_at`; without it every read scanned the
/// whole command log.
#[tokio::test]
async fn whatsapp_statistics_read_their_window_through_an_index() {
    let h = H::new().await;
    let plan: Vec<String> = h
        .ctx
        .sqlite
        .conn()
        .unwrap()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT command, COUNT(*) FROM whatsapp_command_logs
             WHERE executed_at >= ?1 GROUP BY command ORDER BY MIN(id)",
        )
        .unwrap()
        .query_map(["2026-09-26 04:12:00"], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("ix_whatsapp_command_logs_executed_at")),
        "{:?}",
        plan
    );
    h.shutdown().await;
}

/// An analyzer log that cannot be read answers an error, as on the web
/// (`get_analyzer_status_with_auth` and `toggle_analyzer_mode_with_auth`
/// answer 500), never a log of zero. The app's switch does not count the
/// log (web `toggle_analyzer_mode_session`), so it still works.
#[tokio::test]
async fn an_unreadable_analyzer_log_is_an_error_not_zero() {
    let h = H::new().await;
    let app = openalgo_desktop_lib::server::app(h.ctx.clone());
    h.ctx
        .logs
        .conn()
        .unwrap()
        .execute_batch("DROP TABLE analyzer_logs")
        .unwrap();
    let status = send(&h, &app, api(&h, "/api/v1/analyzer", json!({}))).await;
    let v = json_of(&status);
    assert_eq!(status.0, StatusCode::INTERNAL_SERVER_ERROR, "{}", v);
    assert_eq!(v["status"], "error");
    assert_eq!(
        v["message"],
        "The analyzer status could not be read just now. Try again."
    );
    assert!(v.get("data").is_none(), "{}", v);
    let toggle = send(
        &h,
        &app,
        api(&h, "/api/v1/analyzer/toggle", json!({"mode": true})),
    )
    .await;
    let v = json_of(&toggle);
    assert_eq!(toggle.0, StatusCode::INTERNAL_SERVER_ERROR, "{}", v);
    assert_eq!(
        v["message"],
        "Analyzer mode switched to analyze. The analyzer log could not be read just now."
    );
    assert!(
        h.ctx.sqlite.get_analyze_mode().unwrap(),
        "the change was made before the count, as on the web"
    );
    let (c, csrf) = signed_in(&h);
    let mut switch = page("/auth/analyzer-toggle", &c);
    *switch.method_mut() = Method::POST;
    switch
        .headers_mut()
        .insert("x-csrftoken", csrf.parse().unwrap());
    let switched = send(&h, &app, switch).await;
    let v = json_of(&switched);
    assert_eq!(switched.0, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["analyze_mode"], false);
    assert_eq!(v["data"]["mode"], "live");
    h.shutdown().await;
}

/// WhatsApp statistics that cannot be read answer an error. The web answers
/// zero commands, which reads as a bot nobody used (lead, 2026-10-20).
#[tokio::test]
async fn unreadable_whatsapp_statistics_are_an_error_not_zero() {
    let h = H::new().await;
    let (app, c) = (openalgo_desktop_lib::server::app(h.ctx.clone()), cookie(&h));
    h.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute_batch("DROP TABLE whatsapp_command_logs")
        .unwrap();
    let r = send(&h, &app, page("/whatsapp/stats?days=7", &c)).await;
    let v = json_of(&r);
    assert_eq!(r.0, StatusCode::INTERNAL_SERVER_ERROR, "{}", v);
    assert_eq!(v["status"], "error");
    assert!(
        v["message"].as_str().unwrap().contains("could not be read"),
        "{}",
        v
    );
    assert!(v.get("data").is_none(), "{}", v);
    h.shutdown().await;
}
