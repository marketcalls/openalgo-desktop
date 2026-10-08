//! Chartink: the CRUD routes (access, CSRF, shapes, validation), the public
//! webhook's security (rate limit before lookup, per-webhook lockout across
//! addresses, unlock, size cap) and its order semantics, against the real
//! app with the mock broker.

use crate::api_v1_support::H;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openalgo_desktop_lib::brokers::mock::MockCall;
use openalgo_desktop_lib::brokers::types::Position;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

fn session(h: &H) -> (String, String) {
    let s = h.ctx.sessions.create(h.ctx.now());
    h.ctx
        .sessions
        .update(&s.id, |x| x.user = Some("trader".into()));
    (format!("session={}", s.id), s.csrf_token)
}

fn req(
    method: Method,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("accept", "application/json");
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    if let Some(t) = csrf {
        b = b.header("x-csrftoken", t);
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn ip(n: u32) -> IpAddr {
    IpAddr::V4(Ipv4Addr::from(0x0b00_0000 + n))
}

async fn call_from(h: &H, r: Request<Body>, from: IpAddr) -> (StatusCode, Value) {
    let (s, _, b) = h.send_from(r, from).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn call(h: &H, r: Request<Body>) -> (StatusCode, Value) {
    call_from(h, r, IpAddr::V4(Ipv4Addr::LOCALHOST)).await
}

fn placed(h: &H) -> Vec<(String, String, i64)> {
    h.mock
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::PlaceOrder(o) => Some((
                o.symbol.clone(),
                format!("{:?}", o.action).to_uppercase(),
                o.quantity,
            )),
            _ => None,
        })
        .collect()
}

async fn wait_orders(h: &H, n: usize) -> Vec<(String, String, i64)> {
    for _ in 0..600 {
        if placed(h).len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // A little longer, so a second (wrong) order would show up.
    tokio::time::sleep(Duration::from_millis(30)).await;
    placed(h)
}

/// A strategy with SBIN mapped (10, MIS); returns (id, webhook id).
async fn strategy(h: &H, body: Value) -> (i64, String) {
    let (cookie, csrf) = session(h);
    let (s, b) = call(
        h,
        req(
            Method::POST,
            "/chartink/api/strategy",
            Some(body),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let id = b["data"]["strategy_id"].as_i64().unwrap();
    let (s, b) = call(
        h,
        req(
            Method::POST,
            &format!("/chartink/{}/configure", id),
            Some(
                json!({"symbol": "SBIN", "exchange": "NSE", "quantity": 10, "product_type": "MIS"}),
            ),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let (_, b) = call(
        h,
        req(
            Method::GET,
            &format!("/chartink/api/strategy/{}", id),
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    (
        id,
        b["strategy"]["webhook_id"].as_str().unwrap().to_string(),
    )
}

fn positional() -> Value {
    json!({"name": "momentum", "strategy_type": "positional"})
}

fn alert(scan: &str, stocks: &str) -> Value {
    json!({"stocks": stocks, "trigger_prices": "100,200", "triggered_at": "10:00 am",
        "scan_name": scan, "scan_url": "x", "alert_name": "a", "webhook_url": "u"})
}

async fn hook(h: &H, wid: &str, body: Value, from: IpAddr) -> (StatusCode, Value) {
    call_from(
        h,
        req(
            Method::POST,
            &format!("/chartink/webhook/{}", wid),
            Some(body),
            None,
            None,
        ),
        from,
    )
    .await
}

// ------------------------------------------------------------------ CRUD

#[tokio::test]
async fn crud_routes_need_the_user_and_writes_need_csrf() {
    let h = H::new().await;
    for (m, p) in [
        (Method::GET, "/chartink/api/strategies"),
        (Method::GET, "/chartink/api/strategy/1"),
        (Method::POST, "/chartink/api/strategy"),
        (Method::POST, "/chartink/1/delete"),
        (Method::GET, "/chartink/search?q=SB"),
    ] {
        let write = m != Method::GET;
        let (s, _) = call(&h, req(m, p, Some(json!({})), None, None)).await;
        // A write without a session is refused by the CSRF check first.
        let want = if write {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::UNAUTHORIZED
        };
        assert_eq!(s, want, "{}", p);
    }
    let (cookie, _) = session(&h);
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/chartink/api/strategy",
            Some(positional()),
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/chartink/api/strategies",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(b, json!({"strategies": []}));
}

#[tokio::test]
async fn create_validates_like_the_web() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let cases = [
        (json!({}), "No data provided"),
        (json!({"name": ""}), "Strategy name is required"),
        (
            json!({"name": "bad!name"}),
            "Strategy name can only contain letters, numbers, spaces, hyphens and underscores",
        ),
        (
            json!({"name": "x"}),
            "All time fields are required for intraday strategy",
        ),
        (
            json!({"name": "x", "start_time": "10:00", "end_time": "09:00", "squareoff_time": "15:00"}),
            "Start time must be before end time",
        ),
        (
            json!({"name": "x", "start_time": "09:20", "end_time": "15:20", "squareoff_time": "15:10"}),
            "End time must be before square off time",
        ),
        (
            json!({"name": "x", "start_time": "9am", "end_time": "15:00", "squareoff_time": "15:10"}),
            "Invalid time format",
        ),
    ];
    for (body, msg) in cases {
        let (s, b) = call(
            &h,
            req(
                Method::POST,
                "/chartink/api/strategy",
                Some(body.clone()),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", body);
        assert_eq!(b, json!({"status": "error", "message": msg}));
    }
}

#[tokio::test]
async fn strategy_lifecycle_has_the_web_shapes() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let body = json!({"name": "breakout", "start_time": "09:20", "end_time": "15:00", "squareoff_time": "15:15"});
    let (_, b) = call(
        &h,
        req(
            Method::POST,
            "/chartink/api/strategy",
            Some(body),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(b["status"], "success");
    let id = b["data"]["strategy_id"].as_i64().unwrap();

    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/chartink/api/strategies",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    let s = &b["strategies"][0];
    assert_eq!(s["name"], "chartink_breakout");
    assert_eq!(s["is_active"], true);
    assert_eq!(s["is_intraday"], true);
    assert_eq!(s["squareoff_time"], "15:15");
    assert_eq!(s["updated_at"], Value::Null);
    assert!(s["created_at"].as_str().unwrap().contains('T'));
    assert_eq!(s["webhook_id"].as_str().unwrap().len(), 36);
    let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 10, "{:?}", keys);

    // Configure: single, bulk, and the web's errors.
    let cfg = format!("/chartink/{}/configure", id);
    let (s, b) = call(&h, req(Method::POST, &cfg, Some(json!({"symbol": "SBIN", "exchange": "NSE", "quantity": "5", "product_type": "MIS"})), Some(&cookie), Some(&csrf))).await;
    assert_eq!((s, b), (StatusCode::OK, json!({"status": "success"})));
    let (_, b) = call(
        &h,
        req(
            Method::POST,
            &cfg,
            Some(json!({"symbols": "INFY,NSE,3,CNC\nTCS,BSE,2,MIS\n"})),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(b, json!({"status": "success"}));
    for (body, msg) in [
        (
            json!({"symbol": "SBIN", "exchange": "NFO", "quantity": 1, "product_type": "MIS"}),
            "Invalid exchange: NFO",
        ),
        (
            json!({"symbol": "SBIN", "exchange": "NSE", "product_type": "MIS"}),
            "Missing required fields: quantity",
        ),
        (
            json!({"symbol": "SBIN", "exchange": "NSE", "quantity": -1, "product_type": "MIS"}),
            "Quantity must be greater than 0",
        ),
        (
            json!({"symbol": "SBIN", "exchange": "NSE", "quantity": "x", "product_type": "MIS"}),
            "Quantity must be a valid number",
        ),
        (
            json!({"symbols": "SBIN,NSE,1"}),
            "Invalid format in line: SBIN,NSE,1",
        ),
    ] {
        let (s, b) = call(
            &h,
            req(Method::POST, &cfg, Some(body), Some(&cookie), Some(&csrf)),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b, json!({"status": "error", "error": msg}));
    }
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            &format!("/chartink/api/strategy/{}", id),
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    let maps = b["mappings"].as_array().unwrap();
    assert_eq!(maps.len(), 3);
    assert_eq!(maps[0]["chartink_symbol"], "SBIN");
    assert_eq!(maps[0]["quantity"], 5);
    assert_eq!(maps[1]["product_type"], "CNC");
    let mid = maps[0]["id"].as_i64().unwrap();

    h.clock.advance(chrono::Duration::seconds(5));
    let (_, b) = call(
        &h,
        req(
            Method::POST,
            &format!("/chartink/api/strategy/{}/toggle", id),
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(
        b,
        json!({"status": "success", "data": {"is_active": false}})
    );
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            &format!("/chartink/api/strategy/{}", id),
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert!(b["strategy"]["updated_at"].is_string());

    let (_, b) = call(
        &h,
        req(
            Method::POST,
            &format!("/chartink/{}/symbol/{}/delete", id, mid),
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(b, json!({"status": "success"}));
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/chartink/search?q=SBIN&exchange=NSE",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(
        b["results"][0],
        json!({"symbol": "SBIN", "name": "SBIN", "exchange": "NSE"})
    );
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/chartink/search?q=",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(b, json!({"results": []}));

    let (s, b) = call(
        &h,
        req(
            Method::GET,
            "/chartink/api/strategy/999",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(
        (s, b),
        (
            StatusCode::NOT_FOUND,
            json!({"status": "error", "message": "Strategy not found"})
        )
    );
    let (_, b) = call(
        &h,
        req(
            Method::POST,
            &format!("/chartink/{}/delete", id),
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(b, json!({"status": "success"}));
    let (s, b) = call(
        &h,
        req(
            Method::POST,
            &format!("/chartink/{}/delete", id),
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(
        (s, b),
        (
            StatusCode::NOT_FOUND,
            json!({"status": "error", "error": "Strategy not found"})
        )
    );
}

// ------------------------------------------------------------------ webhook semantics

#[tokio::test]
async fn a_valid_buy_alert_places_exactly_one_order() {
    let h = H::new().await;
    let (_, wid) = strategy(&h, positional()).await;
    let (s, b) = hook(&h, &wid, alert("Breakout BUY scan", "SBIN,UNMAPPED"), ip(1)).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(
        b,
        json!({"status": "success", "message": "Orders queued for symbols: SBIN"})
    );
    assert_eq!(
        wait_orders(&h, 1).await,
        vec![("SBIN".into(), "BUY".into(), 10)]
    );
    h.shutdown().await;
}

#[tokio::test]
async fn short_enters_with_a_sell_and_sell_and_cover_exit_with_smart_orders() {
    let h = H::new().await;
    let (_, wid) = strategy(&h, positional()).await;
    let (_, b) = hook(&h, &wid, alert("short-scan", "SBIN"), ip(1)).await;
    assert_eq!(b["status"], "success");
    assert_eq!(
        wait_orders(&h, 1).await,
        vec![("SBIN".into(), "SELL".into(), 10)]
    );

    // COVER: a smart order to flat closes the -10 short with a BUY 10.
    h.mock.calls.lock().clear();
    *h.mock.positions.lock() = Some(Ok(vec![position("SBIN", -10)]));
    hook(&h, &wid, alert("cover", "SBIN"), ip(2)).await;
    assert_eq!(
        wait_orders(&h, 1).await,
        vec![("SBIN".into(), "BUY".into(), 10)]
    );

    // SELL: closes a +25 long, whatever the mapped quantity.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    h.mock.calls.lock().clear();
    *h.mock.positions.lock() = Some(Ok(vec![position("SBIN", 25)]));
    hook(&h, &wid, alert("exit sell", "SBIN"), ip(3)).await;
    assert_eq!(
        wait_orders(&h, 1).await,
        vec![("SBIN".into(), "SELL".into(), 25)]
    );
    h.shutdown().await;
}

fn position(symbol: &str, qty: i32) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: qty,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 100.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: 0,
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

#[tokio::test]
async fn alerts_are_refused_like_the_web() {
    let h = H::new().await;
    // Session clock is 09:42 IST.
    let (_, early) = strategy(&h, json!({"name": "late", "start_time": "10:00", "end_time": "15:00", "squareoff_time": "15:15"})).await;
    let (s, b) = hook(&h, &early, alert("buy", "SBIN"), ip(1)).await;
    assert_eq!(
        (s, b),
        (
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "error": "Cannot place orders before start time"})
        )
    );
    let (_, ended) = strategy(&h, json!({"name": "ended", "start_time": "09:15", "end_time": "09:30", "squareoff_time": "15:15"})).await;
    let (_, b) = hook(&h, &ended, alert("buy", "SBIN"), ip(2)).await;
    assert_eq!(b["error"], "Cannot place entry orders after end time");
    let (s, _) = hook(&h, &ended, alert("cover", "SBIN"), ip(3)).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "an exit is still allowed after the end time"
    );

    h.mock.calls.lock().clear();
    let (id, wid) = strategy(&h, positional()).await;
    let (s, b) = hook(&h, &wid, alert("no keyword", "SBIN"), ip(4)).await;
    assert_eq!(
        (s, b["error"].as_str().unwrap()),
        (
            StatusCode::BAD_REQUEST,
            "No valid action keyword (BUY/SELL/SHORT/COVER) found in scan name"
        )
    );
    let (_, b) = hook(&h, &wid, alert("buy", "UNMAPPED"), ip(5)).await;
    assert_eq!(
        b,
        json!({"status": "warning", "message": "No orders were queued"})
    );
    let (s, b) = call_from(
        &h,
        Request::builder()
            .method(Method::POST)
            .uri(format!("/chartink/webhook/{}", wid))
            .body(Body::empty())
            .unwrap(),
        ip(6),
    )
    .await;
    assert_eq!(
        (s, b),
        (
            StatusCode::BAD_REQUEST,
            json!({"status": "error", "error": "No data received"})
        )
    );
    let (cookie, csrf) = session(&h);
    call(
        &h,
        req(
            Method::POST,
            &format!("/chartink/api/strategy/{}/toggle", id),
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    let (_, b) = hook(&h, &wid, alert("buy", "SBIN"), ip(7)).await;
    assert_eq!(
        b,
        json!({"status": "success", "message": "Strategy is inactive"})
    );
    assert!(
        wait_orders(&h, 1).await.is_empty(),
        "nothing placed by any refused alert"
    );
    h.shutdown().await;
}

// ------------------------------------------------------------------ webhook security

#[tokio::test]
async fn unknown_ids_answer_404_and_a_burst_is_refused_before_lookup() {
    let h = H::new().await;
    let (_, wid) = strategy(&h, positional()).await;
    let attacker = ip(50);
    for i in 0..10 {
        let fake = format!("{:08x}-0000-4000-8000-000000000000", i);
        let (s, b) = hook(&h, &fake, alert("buy", "SBIN"), attacker).await;
        assert_eq!(
            (s, b),
            (
                StatusCode::NOT_FOUND,
                json!({"status": "error", "error": "Invalid webhook ID"})
            )
        );
    }
    let (s, _) = hook(&h, "not-a-uuid", alert("buy", "SBIN"), ip(51)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // The address is now refused before any lookup: even the right id.
    let (s, b) = hook(&h, &wid, alert("buy", "SBIN"), attacker).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(!b.to_string().contains(&wid), "the id is never echoed");
    assert!(wait_orders(&h, 1).await.is_empty());

    // The per-address rate limit: 100 a minute, then refused.
    let busy = ip(60);
    let (cookie, csrf) = session(&h);
    let (id2, wid2) = strategy(&h, positional()).await;
    call(
        &h,
        req(
            Method::POST,
            &format!("/chartink/api/strategy/{}/toggle", id2),
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    for _ in 0..100 {
        let (s, _) = hook(&h, &wid2, alert("buy", "SBIN"), busy).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _) = hook(&h, &wid2, alert("buy", "SBIN"), busy).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // Bounded windows.
    assert!(h.ctx.chartink.guard.tracked() < 50);
    h.shutdown().await;
}

#[tokio::test]
async fn wrong_ids_from_rotating_addresses_lock_the_webhook_until_unlocked() {
    let h = H::new().await;
    let (id, wid) = strategy(&h, positional()).await;
    let locator = &wid[..8];
    for n in 0..10 {
        let wrong = format!("{}-ffff-4fff-8fff-{:012x}", locator, n);
        let (s, _) = hook(&h, &wrong, alert("buy", "SBIN"), ip(100 + n)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let (s, b) = hook(&h, &wid, alert("buy", "SBIN"), ip(200)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(b["error"]
        .as_str()
        .unwrap()
        .contains("turn the strategy off and on again"));
    assert!(
        wait_orders(&h, 1).await.is_empty(),
        "a locked webhook places nothing"
    );

    // The trader turns the strategy off and on: unlocked.
    let (cookie, csrf) = session(&h);
    for _ in 0..2 {
        call(
            &h,
            req(
                Method::POST,
                &format!("/chartink/api/strategy/{}/toggle", id),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
    }
    let (s, b) = hook(&h, &wid, alert("buy", "SBIN"), ip(201)).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(wait_orders(&h, 1).await.len(), 1);

    // Locked again, the lock also lapses on its own.
    for n in 0..10 {
        let wrong = format!("{}-eeee-4eee-8eee-{:012x}", locator, n);
        hook(&h, &wrong, alert("buy", "SBIN"), ip(300 + n)).await;
    }
    let (s, _) = hook(&h, &wid, alert("buy", "SBIN"), ip(400)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    h.ctx
        .chartink
        .guard
        .advance(openalgo_desktop_lib::chartink::webhook::LOCK_DURATION);
    let (s, _) = hook(&h, &wid, alert("buy", "SBIN"), ip(401)).await;
    assert_eq!(s, StatusCode::OK);
    h.shutdown().await;
}

#[tokio::test]
async fn an_oversized_body_is_refused_unread() {
    let h = H::new().await;
    let (_, wid) = strategy(&h, positional()).await;
    let big = json!({"scan_name": "buy", "stocks": "SBIN", "pad": "x".repeat(20_000)});
    let (s, _) = hook(&h, &wid, big, ip(1)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(wait_orders(&h, 1).await.is_empty());
    h.shutdown().await;
}

#[tokio::test]
async fn intraday_strategies_are_squared_off_once_per_day() {
    let h = H::new().await;
    let (id, _) = strategy(&h, json!({"name": "intra", "start_time": "09:15", "end_time": "09:30", "squareoff_time": "09:40"})).await;
    *h.mock.positions.lock() = Some(Ok(vec![position("SBIN", 10)]));
    let now = h.ctx.now(); // 09:42 IST: two minutes late, inside the grace
    assert_eq!(h.ctx.chartink.run_due_squareoffs(now), vec![id]);
    assert!(
        h.ctx.chartink.run_due_squareoffs(now).is_empty(),
        "once a day"
    );
    assert_eq!(
        wait_orders(&h, 1).await,
        vec![("SBIN".into(), "SELL".into(), 10)]
    );
    let later = now + chrono::Duration::minutes(30);
    assert!(h.ctx.chartink.run_due_squareoffs(later).is_empty());
    let chartink = h.ctx.chartink.clone();
    h.shutdown().await;
    assert_eq!(chartink.task_count(), 0, "worker and scheduler stopped");
}

#[tokio::test]
async fn migration_backfills_legacy_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT);
             INSERT INTO users (username) VALUES ('trader');
             CREATE TABLE chartink_strategies (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, webhook_id TEXT NOT NULL UNIQUE,
                scan_url TEXT, product TEXT NOT NULL DEFAULT 'MIS', quantity INTEGER NOT NULL DEFAULT 1,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')), updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE chartink_symbol_mappings (
                id INTEGER PRIMARY KEY AUTOINCREMENT, strategy_id INTEGER NOT NULL, exchange TEXT NOT NULL,
                symbol TEXT NOT NULL, quantity INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO chartink_strategies (name, webhook_id, product, enabled) VALUES ('old', 'w-1', 'CNC', 0);
             INSERT INTO chartink_symbol_mappings (strategy_id, exchange, symbol, quantity) VALUES (1, 'NSE', 'SBIN', 3);",
        )
        .unwrap();
        openalgo_desktop_lib::chartink::store::migrate(&conn).unwrap();
        // Idempotent.
        openalgo_desktop_lib::chartink::store::migrate(&conn).unwrap();
        let (active, user, intraday): (i64, String, i64) = conn
            .query_row(
                "SELECT is_active, user_id, is_intraday FROM chartink_strategies",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((active, user.as_str(), intraday), (0, "trader", 0));
        let (sym, product): (String, String) = conn
            .query_row(
                "SELECT chartink_symbol, product_type FROM chartink_symbol_mappings",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((sym.as_str(), product.as_str()), ("SBIN", "CNC"));
    }
}
