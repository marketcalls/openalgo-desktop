//! The scalping terminal backend: `/scalping/api` routes (access, CSRF,
//! shapes, validation) against the real app with the mock broker, and the
//! risk monitor with an injected clock, ticks, gateway and price source.

use crate::api_v1_support::H;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::TimeZone;
use openalgo_desktop_lib::brokers::common::streaming::MarketEvent;
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::mock::MockCall;
use openalgo_desktop_lib::brokers::types::Position;
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::db::sqlite::SqliteDb;
use openalgo_desktop_lib::events::subscribers::socketio::UiEmitter;
use openalgo_desktop_lib::scalping::monitor::{chunks, MonitorDeps, RiskMonitor};
use openalgo_desktop_lib::scalping::store::{SlUpsert, Store};
use openalgo_desktop_lib::services::order_reconciler::BookSnapshot;
use openalgo_desktop_lib::strategy::dispatch::{
    Book, BookOrder, DispatchResult, OrderGateway, OrderPayload, OrderStatusResult, RunMode,
};
use openalgo_desktop_lib::strategy::tick_feed::{Key, PriceSource};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

const OPT: &str = "NIFTY06OCT2622000CE";

// ------------------------------------------------------------------ route harness

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

async fn call(h: &H, r: Request<Body>) -> (StatusCode, Value) {
    let (s, _, b) = h.send_from(r, IpAddr::V4(Ipv4Addr::LOCALHOST)).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn pos(symbol: &str, exchange: &str, product: &str, qty: i32) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: exchange.into(),
        product: product.into(),
        quantity: qty,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 100.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: qty.max(0),
        buy_value: 0.0,
        sell_quantity: (-qty).max(0),
        sell_value: 0.0,
    }
}

fn placed(h: &H) -> Vec<(String, i64)> {
    h.mock
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::PlaceOrder(o) => Some((format!("{:?}", o.action).to_uppercase(), o.quantity)),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------ routes

#[tokio::test]
async fn routes_need_the_signed_in_user() {
    let h = H::new().await;
    for (m, p) in [
        (Method::GET, "/scalping/api/underlyings"),
        (Method::GET, "/scalping/api/sl"),
        (Method::POST, "/scalping/api/order"),
        (Method::DELETE, "/scalping/api/tracked"),
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
}

#[tokio::test]
async fn writes_need_the_csrf_token() {
    let h = H::new().await;
    let (cookie, _) = session(&h);
    let body = json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "quantity": 65});
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/order",
            Some(body),
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(placed(&h).is_empty(), "no order without the token");
}

#[tokio::test]
async fn underlyings_and_lookups_have_the_web_shapes() {
    let h = H::new().await;
    let (cookie, _) = session(&h);
    let (s, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/underlyings",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["status"], "success");
    assert_eq!(
        b["data"][0],
        json!({"underlying": "NIFTY", "index_exchange": "NSE_INDEX", "fo_exchange": "NFO"})
    );
    assert_eq!(b["data"].as_array().unwrap().len(), 7);

    let (s, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/expiry?underlying=NIFTY&exchange=NFO",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let first = b["data"][0].as_str().unwrap();
    assert!(!first.contains('-') && first.len() == 7, "{}", first);

    let (s, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/expiry?underlying=NIFTY&exchange=NSE",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b,
        json!({"status": "error", "message": "Invalid exchange: NSE"})
    );

    let (s, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/futures?underlying=NIFTY&exchange=NFO",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let f = &b["data"][0];
    for k in ["symbol", "expiry", "lotsize", "tick_size"] {
        assert!(f.get(k).is_some(), "{}", k);
    }

    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/search?exchange=NSE&query=S",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(b, json!({"status": "success", "data": []}));

    let (s, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/strikes?underlying=NIFTY&exchange=NFO",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["message"], "expiry parameter is required");

    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/all_underlyings?exchange=NFO",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    let list = b["data"].as_array().unwrap();
    assert_eq!(list[0], "BANKNIFTY", "indices first, sorted: {:?}", list);
}

#[tokio::test]
async fn order_validation_matches_the_web() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let cases = [
        (
            json!({"exchange": "NFO", "action": "BUY", "quantity": 65}),
            "symbol is required",
        ),
        (
            json!({"symbol": OPT, "exchange": "XYZ", "action": "BUY", "quantity": 65}),
            "Invalid exchange: XYZ",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "HOLD", "quantity": 65}),
            "Invalid action: HOLD",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "product": "CNC", "quantity": 65}),
            "Invalid product for NFO: CNC",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "quantity": 0}),
            "quantity must be positive",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "quantity": 100}),
            "quantity must be a whole number of lots (lot size 65)",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "quantity": 65 * 21}),
            "quantity exceeds the 20-lot cap",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "quantity": 65, "lots": 21}),
            "lots must be between 1 and 20",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "quantity": 200000}),
            "quantity exceeds the safety limit",
        ),
    ];
    for (body, msg) in cases {
        let (s, b) = call(
            &h,
            req(
                Method::POST,
                "/scalping/api/order",
                Some(body.clone()),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", body);
        assert_eq!(b, json!({"status": "error", "message": msg}));
    }
    assert!(placed(&h).is_empty());
}

#[tokio::test]
async fn an_entry_is_placed_and_tracked_in_its_mode() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let body = json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "product": "NRML", "quantity": 130, "lots": 2});
    let (s, b) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/order",
            Some(body),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(b["status"], "success");
    assert_eq!(placed(&h), vec![("BUY".to_string(), 130)]);

    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/tracked",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(
        b,
        json!({"status": "success", "data": [{"symbol": OPT, "exchange": "NFO", "product": "NRML", "mode": "live"}]})
    );
    // The sandbox list is separate.
    h.analyze(true);
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/tracked",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(b["data"], json!([]));
    h.analyze(false);
    let (_, b) = call(
        &h,
        req(
            Method::DELETE,
            "/scalping/api/tracked",
            None,
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(b, json!({"status": "success", "cleared": true}));
    h.shutdown().await;
}

#[tokio::test]
async fn close_all_flattens_only_the_scalping_list_freeze_safe() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let (_, b) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/close_all",
            Some(json!({})),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(
        b,
        json!({"status": "success", "message": "No scalping positions to close", "results": []})
    );

    h.ctx
        .scalping
        .store
        .track(OPT, "NFO", "NRML", "live")
        .unwrap();
    // 30 lots: above the NIFTY freeze (1800 -> 27 whole lots = 1755).
    *h.mock.positions.lock() = Some(Ok(vec![
        pos(OPT, "NFO", "NRML", 65 * 30),
        pos("SBIN", "NSE", "MIS", 10),
    ]));
    let (s, b) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/close_all",
            Some(json!({})),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(b["message"], "Closed 1 scalping position(s)");
    assert_eq!(b["results"][0]["symbol"], OPT);
    assert_eq!(b["results"][0]["status"], "success");
    assert_eq!(
        placed(&h),
        vec![("SELL".to_string(), 1755), ("SELL".to_string(), 195)],
        "split into freeze-sized whole-lot chunks; SBIN untouched"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn close_leg_validates_and_refuses_part_lots() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let (s, b) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/close_leg",
            Some(json!({"symbol": OPT, "exchange": "NFO", "action": "SELL", "quantity": 50})),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["message"], "quantity must be a whole number of lots (65)");
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/close_leg",
            Some(json!({"symbol": OPT, "exchange": "NFO", "action": "SELL", "quantity": 65})),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(placed(&h), vec![("SELL".to_string(), 65)]);
}

#[tokio::test]
async fn stop_loss_states_round_trip_with_validation() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let bad = [
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "CNC"}),
            "Invalid symbol/exchange/product",
        ),
        (
            json!({"symbol": OPT, "exchange": "NSE", "product": "MIS"}),
            "Invalid symbol/exchange/product",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "side": "LONG"}),
            "Invalid side: LONG",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "quantity": "x"}),
            "quantity must be an integer",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "quantity": -1}),
            "quantity out of range",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "initial_sl": "abc"}),
            "initial_sl must be a number",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "target": -5}),
            "target out of range",
        ),
        (
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "current_sl": "nan"}),
            "current_sl out of range",
        ),
    ];
    for (body, msg) in bad {
        let (s, b) = call(
            &h,
            req(
                Method::POST,
                "/scalping/api/sl",
                Some(body),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["message"], msg);
    }
    let body = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "side": "BUY", "quantity": 65,
        "entry_price": 100.5, "initial_sl": 95, "trailing_enabled": true, "trailing_step": 2, "target": 120});
    let (s, b) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/sl",
            Some(body),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let want = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "mode": "live", "side": "BUY",
        "entry_price": 100.5, "quantity": 65, "initial_sl": 95.0, "trailing_enabled": true, "trailing_step": 2.0,
        "highest_price": null, "lowest_price": null, "current_sl": null, "target": 120.0, "is_active": true});
    assert_eq!(b, json!({"status": "success", "data": want}));
    let (_, b) = call(
        &h,
        req(Method::GET, "/scalping/api/sl", None, Some(&cookie), None),
    )
    .await;
    assert_eq!(b, json!({"status": "success", "data": [want]}));
    // Another mode does not see it.
    h.analyze(true);
    let (_, b) = call(
        &h,
        req(Method::GET, "/scalping/api/sl", None, Some(&cookie), None),
    )
    .await;
    assert_eq!(b["data"], json!([]));
    let del = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS"});
    let (_, b) = call(
        &h,
        req(
            Method::DELETE,
            "/scalping/api/sl",
            Some(del.clone()),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(
        b,
        json!({"status": "success", "deleted": false}),
        "the sandbox delete leaves the live stop"
    );
    h.analyze(false);
    let (_, b) = call(
        &h,
        req(
            Method::DELETE,
            "/scalping/api/sl",
            Some(del),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(b, json!({"status": "success", "deleted": true}));
}

// ------------------------------------------------------------------ monitor fakes

#[derive(Default)]
struct FakeGateway {
    refuse: Mutex<bool>,
    /// Placements answer with no definite outcome (LOG-08).
    uncertain: Mutex<bool>,
    /// Per placement, front first: `None` accepts, `Some(reason)` refuses.
    script: Mutex<std::collections::VecDeque<Option<&'static str>>>,
    delay_ms: Mutex<u64>,
    net: Mutex<i64>,
    /// The position book has no row for the leg (empty or lagging).
    empty: Mutex<bool>,
    placed: Mutex<Vec<(RunMode, OrderPayload)>>,
    books: Mutex<Vec<RunMode>>,
    /// While set, every position-book read waits on it (barrier).
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    entered: tokio::sync::Notify,
}

impl FakeGateway {
    fn gate(&self) -> Arc<tokio::sync::Notify> {
        let g = Arc::new(tokio::sync::Notify::new());
        *self.gate.lock() = Some(g.clone());
        g
    }

    fn release(&self) {
        *self.gate.lock() = None;
    }
}

#[async_trait]
impl OrderGateway for FakeGateway {
    async fn place(&self, mode: RunMode, order: &OrderPayload) -> DispatchResult {
        let d = *self.delay_ms.lock();
        if d > 0 {
            tokio::time::sleep(Duration::from_millis(d)).await;
        }
        let n = {
            let mut p = self.placed.lock();
            p.push((mode, order.clone()));
            p.len()
        };
        if *self.refuse.lock() {
            return DispatchResult::refused("Broker rejected the order");
        }
        if *self.uncertain.lock() {
            return DispatchResult {
                ok: false,
                error: Some("The broker did not confirm whether this order was placed.".into()),
                uncertain: true,
                ..Default::default()
            };
        }
        if let Some(Some(reason)) = self.script.lock().pop_front() {
            return DispatchResult::refused(reason);
        }
        DispatchResult {
            ok: true,
            broker_order_id: Some(format!("X{}", n)),
            response: json!({}),
            ..Default::default()
        }
    }
    async fn cancel(&self, _: RunMode, _: &str) -> DispatchResult {
        DispatchResult::refused("unused")
    }
    async fn order_status(&self, _: RunMode, _: &str) -> OrderStatusResult {
        OrderStatusResult::default()
    }
    fn authorised(&self, _: RunMode) -> Result<(), String> {
        Ok(())
    }
    fn broker_name(&self, _: RunMode) -> String {
        "fake".into()
    }
    async fn book(&self, mode: RunMode, _: Book) -> Result<Value, Value> {
        self.books.lock().push(mode);
        let gate = self.gate.lock().clone();
        if let Some(g) = gate {
            self.entered.notify_one();
            g.notified().await;
        }
        if *self.empty.lock() {
            return Ok(json!({"status": "success", "data": []}));
        }
        Ok(json!({"status": "success", "data": [
            {"symbol": OPT, "exchange": "NFO", "product": "MIS", "quantity": *self.net.lock()}
        ]}))
    }
    async fn ltp(&self, _: &str, _: &str) -> Result<f64, String> {
        Err("unused".into())
    }
}

struct FakePrices {
    tx: broadcast::Sender<MarketEvent>,
    held: Mutex<Vec<Key>>,
}

impl FakePrices {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tx: broadcast::channel(64).0,
            held: Mutex::new(vec![]),
        })
    }
}

#[async_trait]
impl PriceSource for FakePrices {
    async fn subscribe(&self, keys: &[Key]) {
        self.held.lock().extend(keys.iter().cloned());
    }
    async fn unsubscribe(&self, keys: &[Key]) {
        self.held.lock().retain(|k| !keys.contains(k));
    }
    fn ticks(&self) -> broadcast::Receiver<MarketEvent> {
        self.tx.subscribe()
    }
    async fn poll(&self, _: &[Key]) -> Vec<(Key, f64)> {
        vec![]
    }
}

#[derive(Default)]
struct Ui {
    events: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl UiEmitter for Ui {
    async fn emit(&self, event: &str, payload: Value) {
        self.events.lock().push((event.to_string(), payload));
    }
}

struct M {
    mon: Arc<RiskMonitor>,
    store: Store,
    db: Arc<SqliteDb>,
    /// Trader alerts the monitor raised.
    alerts: Arc<Mutex<Vec<String>>>,
    gw: Arc<FakeGateway>,
    prices: Arc<FakePrices>,
    ui: Arc<Ui>,
    clock: Arc<ManualClock>,
    _dir: tempfile::TempDir,
}

fn monitor() -> M {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(SqliteDb::new(&dir.path().join("t.db")).unwrap());
    let store = Store::new(db.clone());
    let symbols = SymbolResolver::new();
    symbols.load(crate::api_v1_support::master());
    let gw = Arc::new(FakeGateway::default());
    *gw.net.lock() = 65;
    let prices = FakePrices::new();
    let ui = Arc::new(Ui::default());
    let clock = ManualClock::new(chrono::Utc.with_ymd_and_hms(2026, 10, 5, 5, 0, 0).unwrap());
    let mon = RiskMonitor::new(MonitorDeps {
        store: store.clone(),
        gateway: gw.clone(),
        prices: Some(prices.clone()),
        ui: ui.clone(),
        clock: clock.clone(),
        symbols,
    });
    let alerts = Arc::new(Mutex::new(Vec::new()));
    let sink = alerts.clone();
    mon.set_alert_sink(Arc::new(move |m: String| sink.lock().push(m)));
    M {
        mon,
        store,
        db,
        alerts,
        gw,
        prices,
        ui,
        clock,
        _dir: dir,
    }
}

fn leg(mode: &str) -> SlUpsert {
    SlUpsert {
        symbol: OPT.into(),
        exchange: "NFO".into(),
        product: "MIS".into(),
        mode: mode.into(),
        side: Some("BUY".into()),
        entry_price: Some(100.0),
        quantity: Some(65),
        initial_sl: Some(95.0),
        is_active: Some(true),
        ..Default::default()
    }
}

// ------------------------------------------------------------------ monitor

fn book(rows: Vec<Value>, now: chrono::DateTime<chrono::Utc>) -> BookSnapshot {
    BookSnapshot::new(rows.iter().filter_map(BookOrder::from_row).collect(), now)
}

fn order_row(id: &str, action: &str, qty: i64, status: &str) -> Value {
    json!({
        "orderid": id, "symbol": OPT, "exchange": "NFO", "action": action,
        "quantity": qty, "product": "MIS", "order_status": status,
        "filled_quantity": if status == "complete" { qty } else { 0 },
        "average_price": 94.5, "timestamp": "",
    })
}

fn cleared(m: &M) -> bool {
    m.ui.events
        .lock()
        .iter()
        .any(|(_, p)| p["cleared"] == json!(true))
}

/// Replaces `stop_loss_exits_once_and_clears_the_leg` (LOG-05): an
/// accepted exit is not a flat position. The stop, its row and its price
/// subscription stay until the exit has filled and the book shows flat.
#[tokio::test]
async fn stop_loss_exit_keeps_protection_until_it_fills_and_the_leg_is_flat() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    assert_eq!(
        m.mon.subscribed(),
        vec![(OPT.to_string(), "NFO".to_string())]
    );
    m.mon.process_tick(OPT, "NFO", 96.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty());
    m.mon.process_tick(OPT, "NFO", 95.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1);
    assert_eq!(
        placed[0].0,
        RunMode::Live,
        "a live leg exits live (force_live)"
    );
    assert_eq!(
        (placed[0].1.action.as_str(), placed[0].1.quantity),
        ("SELL", 65)
    );
    assert_eq!(placed[0].1.strategy, "Scalping");
    // Accepted, not filled: still protected.
    assert_eq!(m.mon.leg_count(), 1);
    assert_eq!(m.store.active_sl(None).unwrap().len(), 1);
    assert_eq!(m.mon.subscribed().len(), 1);
    assert!(!cleared(&m), "no clear on acceptance");
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert_eq!(a.children[0].order_id.as_deref(), Some("X1"));
    assert!(
        m.store.active_sl(None).unwrap()[0].exit_attempt.is_some(),
        "the attempt is on the row (survives a restart)"
    );
    // The exit covers the position: more breaching ticks send nothing.
    m.clock.advance(chrono::Duration::seconds(10));
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    // The order book shows it filled and the position book flat: cleared.
    *m.gw.net.lock() = 0;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(vec![order_row("X1", "SELL", 65, "complete")], m.clock.now()),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 0);
    assert!(m.store.active_sl(None).unwrap().is_empty());
    assert!(
        m.mon.subscribed().is_empty(),
        "subscription released with the leg"
    );
    let ev = m.ui.events.lock().clone();
    assert_eq!(
        ev.last().unwrap(),
        &(
            "scalping_sl_update".to_string(),
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "cleared": true})
        )
    );
    assert_eq!(m.mon.tracked_entries(), 0);
}

/// LOG-05 trigger A: accepted, then rejected by the exchange. The leg stays
/// protected and the stop exits again after the cooldown.
#[tokio::test]
async fn an_exit_rejected_after_acceptance_keeps_the_leg_and_exits_again() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(vec![order_row("X1", "SELL", 65, "rejected")], m.clock.now()),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 1, "still managed");
    assert_eq!(m.store.active_sl(None).unwrap().len(), 1);
    assert!(!cleared(&m));
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 2);
    assert_eq!(placed[1].1.quantity, 65, "the whole position, again");
}

/// LOG-05 trigger B: a freeze-split exit whose first chunk is working and
/// second refused, with the position book unchanged. The retry sends only
/// the refused part, never the whole position again.
#[tokio::test]
async fn a_partly_refused_split_exit_retries_only_what_is_not_working() {
    let m = monitor();
    *m.gw.net.lock() = 1950;
    let mut l = leg("live");
    l.quantity = Some(1950);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    // First chunk accepted, second refused.
    m.gw.script.lock().push_back(None);
    m.gw.script.lock().push_back(Some("Exchange busy"));
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(
        placed.iter().map(|p| p.1.quantity).collect::<Vec<_>>(),
        vec![1755, 195]
    );
    // A delayed partial fill of the first chunk, and the book follows it.
    *m.gw.net.lock() = 950;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(
                vec![json!({
                    "orderid": "X1", "symbol": OPT, "exchange": "NFO", "action": "SELL",
                    "quantity": 1755, "product": "MIS", "order_status": "open",
                    "filled_quantity": 1000, "average_price": 94.5, "timestamp": "",
                })],
                m.clock.now(),
            ),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 1, "partly filled: still protected");
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 3);
    assert_eq!(
        placed[2].1.quantity, 195,
        "1950 less 1000 filled less the 755 still working"
    );
    // Total exit quantity sent never exceeds the position.
    let accepted: i64 = [placed[0].1.quantity, placed[2].1.quantity].iter().sum();
    assert_eq!(accepted, 1950);
    // The working chunk covers the rest: further breaches send nothing.
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 92.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 3);
    // Both fill; the book catches up: flat and cleared, nothing more sent.
    *m.gw.net.lock() = 0;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(
                vec![
                    order_row("X1", "SELL", 1755, "complete"),
                    order_row("X3", "SELL", 195, "complete"),
                ],
                m.clock.now(),
            ),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 0);
    assert_eq!(m.gw.placed.lock().len(), 3);
}

/// LOG-08: an exit whose answer was lost is not sent again; the book
/// settles it.
#[tokio::test]
async fn an_uncertain_scalping_exit_is_not_sent_again_until_the_book_settles_it() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    *m.gw.uncertain.lock() = true;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    *m.gw.uncertain.lock() = false;
    for s in 0..5 {
        m.clock.advance(chrono::Duration::seconds(4));
        m.mon.process_tick(OPT, "NFO", 93.0 - s as f64).await;
        m.mon.wait_idle().await;
    }
    assert_eq!(m.gw.placed.lock().len(), 1, "never a second exit");
    // The broker had it, filled.
    *m.gw.net.lock() = 0;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(
                vec![order_row("BRK-7", "SELL", 65, "complete")],
                m.clock.now(),
            ),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 0);
    assert_eq!(m.gw.placed.lock().len(), 1);
}

/// LOG-05: a restart while an exit is pending reloads its orders from the
/// stop's row: no second exit, and the book still settles it.
#[tokio::test]
async fn a_restart_while_an_exit_is_pending_sends_no_second_exit() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    // The app stops (memory gone) and starts again (rows reloaded).
    m.mon.stop().await;
    assert_eq!(m.mon.leg_count(), 0);
    m.mon.sync().await;
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert_eq!(a.children[0].order_id.as_deref(), Some("X1"));
    m.clock.advance(chrono::Duration::seconds(10));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(
        m.gw.placed.lock().len(),
        1,
        "no second exit after a restart"
    );
    *m.gw.net.lock() = 0;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(vec![order_row("X1", "SELL", 65, "complete")], m.clock.now()),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 0);
    assert!(m.store.active_sl(None).unwrap().is_empty());
}

/// LOG-06: an exit decided on a stop that is edited while the exit reads
/// the position book sends nothing, and never deletes the edited stop.
#[tokio::test]
async fn an_edit_during_an_exit_keeps_the_new_stop() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    let gate = m.gw.gate();
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.gw.entered.notified().await;
    // The trader moves the stop while the exit is in flight.
    let mut edit = leg("live");
    edit.initial_sl = Some(90.0);
    edit.current_sl = Some(90.0);
    m.store.upsert_sl(&edit).unwrap();
    m.mon.sync().await;
    gate.notify_one();
    m.mon.wait_idle().await;
    assert!(
        m.gw.placed.lock().is_empty(),
        "a stale decision sends nothing"
    );
    let rows = m.store.active_sl(None).unwrap();
    assert_eq!(rows.len(), 1, "the new stop survives");
    assert_eq!(rows[0].current_sl, Some(90.0));
}

/// LOG-06: delete and re-add a stop while its exit is in flight: the old
/// worker sends nothing, no second worker starts beside it, and the next
/// breach sends exactly one exit.
#[tokio::test]
async fn delete_and_re_add_during_an_exit_sends_exactly_one_exit() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    let gate = m.gw.gate();
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.gw.entered.notified().await;
    m.store.delete_sl(OPT, "NFO", "MIS", Some("live")).unwrap();
    m.mon.sync().await;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    // A breach while the old worker is still in flight starts no second one.
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.gw.release();
    gate.notify_one();
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty());
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1, "exactly one exit");
}

#[tokio::test]
async fn target_exits_a_short_leg_with_a_buy() {
    let m = monitor();
    *m.gw.net.lock() = -130;
    let mut l = leg("live");
    l.side = Some("SELL".into());
    l.initial_sl = Some(110.0);
    l.target = Some(90.0);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 95.0).await;
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1);
    assert_eq!(
        (placed[0].1.action.as_str(), placed[0].1.quantity),
        ("BUY", 130)
    );
}

#[tokio::test]
async fn trailing_stop_ratchets_persists_pushes_then_exits() {
    let m = monitor();
    let mut l = leg("analyze");
    l.trailing_enabled = Some(true);
    l.trailing_step = Some(5.0);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 110.0).await;
    let s = m.mon.leg("analyze", OPT, "NFO", "MIS").unwrap();
    assert_eq!(s.current_sl, Some(105.0));
    assert_eq!(s.highest_price, Some(110.0));
    assert_eq!(
        m.store.active_sl(None).unwrap()[0].current_sl,
        Some(105.0),
        "persisted"
    );
    assert_eq!(
        m.ui.events.lock()[0],
        (
            "scalping_sl_update".to_string(),
            json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "cleared": false, "current_sl": 105.0, "target": null})
        )
    );
    // Inside the throttle: moved in memory, not written or pushed again.
    m.mon.process_tick(OPT, "NFO", 112.0).await;
    assert_eq!(
        m.mon.leg("analyze", OPT, "NFO", "MIS").unwrap().current_sl,
        Some(107.0)
    );
    assert_eq!(m.store.active_sl(None).unwrap()[0].current_sl, Some(105.0));
    assert_eq!(m.ui.events.lock().len(), 1);
    // A pull-back never loosens the stop; through it, the leg exits.
    m.mon.process_tick(OPT, "NFO", 108.0).await;
    assert!(m.gw.placed.lock().is_empty());
    m.mon.process_tick(OPT, "NFO", 107.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1);
    assert_eq!(
        placed[0].0,
        RunMode::Sandbox,
        "a sandbox leg exits to the sandbox"
    );
}

#[tokio::test]
async fn concurrent_ticks_send_exactly_one_exit() {
    let m = monitor();
    *m.gw.delay_ms.lock() = 50;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    let mut set = tokio::task::JoinSet::new();
    for i in 0..64 {
        let mon = m.mon.clone();
        set.spawn(async move { mon.process_tick(OPT, "NFO", 90.0 - (i % 3) as f64).await });
    }
    while set.join_next().await.is_some() {}
    m.mon.wait_idle().await;
    // Ticks after the exit find it covered by the working exit order.
    m.mon.process_tick(OPT, "NFO", 80.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    assert_eq!(m.gw.books.lock().len(), 1);
}

#[tokio::test]
async fn a_refused_exit_keeps_the_leg_managed_and_retries_after_cooldown() {
    let m = monitor();
    *m.gw.refuse.lock() = true;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    assert_eq!(m.mon.leg_count(), 1, "still managed");
    assert_eq!(m.store.active_sl(None).unwrap().len(), 1);
    assert_eq!(m.mon.subscribed().len(), 1);
    // Within the cooldown nothing is sent again.
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    m.clock.advance(chrono::Duration::seconds(4));
    *m.gw.refuse.lock() = false;
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 2);
    // Accepted is not flat: still protected until the exit fills (LOG-05).
    assert_eq!(m.mon.leg_count(), 1);
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert_eq!(a.children.len(), 1, "the refused exit left no child");
    assert_eq!(a.children[0].order_id.as_deref(), Some("X2"));
}

#[tokio::test]
async fn an_already_flat_leg_is_cleared_without_an_order() {
    let m = monitor();
    *m.gw.net.lock() = 0;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty());
    assert_eq!(m.mon.leg_count(), 0);
}

/// Monitor fail-open review, 1: a position book without the leg's row
/// (empty or lagging right after an entry) is not a flat position. The
/// stop stays, nothing is sent, and the trader is alerted once.
#[tokio::test]
async fn an_empty_position_book_right_after_entry_keeps_protection() {
    let m = monitor();
    *m.gw.empty.lock() = true;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert!(
        m.gw.placed.lock().is_empty(),
        "no quantity to exit is known"
    );
    assert_eq!(m.mon.leg_count(), 1, "still protected");
    assert_eq!(m.store.active_sl(None).unwrap().len(), 1);
    assert!(!cleared(&m));
    assert_eq!(m.alerts.lock().len(), 1);
    // The book catches up with the entry: the next breach exits it.
    *m.gw.empty.lock() = false;
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 89.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].1.quantity, 65);
    assert_eq!(m.alerts.lock().len(), 1, "alerted once");
}

/// Monitor fail-open review, 1: exit orders that were all rejected, with
/// a position book that does not show the leg (or shows a zero the exits
/// did not make), never clear the stop.
#[tokio::test]
async fn rejected_exit_orders_and_an_empty_book_keep_protection() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    *m.gw.empty.lock() = true;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(vec![order_row("X1", "SELL", 65, "rejected")], m.clock.now()),
        )
        .await;
    assert_eq!(m.mon.leg_count(), 1, "an empty book is not flat");
    assert!(!cleared(&m));
    // A zero row the rejected exit cannot have made: still not flat.
    *m.gw.empty.lock() = false;
    *m.gw.net.lock() = 0;
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.mon.leg_count(), 1);
    assert!(!cleared(&m));
    assert_eq!(m.gw.placed.lock().len(), 1, "nothing to exit is shown");
    // The book shows the position again: the stop exits it.
    *m.gw.net.lock() = 65;
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 92.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 2);
}

/// Monitor fail-open review, 2: exit orders stored on the stop's row that
/// cannot be read back are never taken as "no exit sent". No exit goes
/// out and the trader is alerted.
#[tokio::test]
async fn an_unreadable_stored_exit_attempt_fails_closed() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    let id = m.store.active_sl(None).unwrap()[0].id;
    m.store
        .set_exit_attempt(
            id,
            Some("{\"id\":\"a1\",\"net0\":65,\"children\":[{\"order_id\""),
        )
        .unwrap();
    m.mon.sync().await;
    assert_eq!(m.mon.leg_count(), 1);
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty(), "never a blind exit");
    assert_eq!(m.mon.leg_count(), 1, "still protected");
    let alerts = m.alerts.lock().clone();
    assert_eq!(alerts.len(), 1);
    assert!(
        alerts[0].contains("could not be read back"),
        "{}",
        alerts[0]
    );
    // Another sync does not alert again.
    m.mon.sync().await;
    assert_eq!(m.alerts.lock().len(), 1);
}

/// Monitor fail-open review, 3: an exit whose orders cannot be saved is
/// not started, and a split exit stops after the first order it could not
/// save.
#[tokio::test]
async fn an_exit_that_cannot_be_saved_is_not_sent() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.db.conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER no_attempt BEFORE UPDATE OF exit_attempt ON scalping_sl_state
             BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
        )
        .unwrap();
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty());
    assert_eq!(m.mon.leg_count(), 1);
    assert_eq!(m.alerts.lock().len(), 1);
}

#[tokio::test]
async fn a_split_exit_stops_at_the_first_order_it_cannot_save() {
    let m = monitor();
    *m.gw.net.lock() = 1950;
    let mut l = leg("live");
    l.quantity = Some(1950);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    // The empty attempt saves; one that records a sent order does not.
    m.db.conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER no_child BEFORE UPDATE OF exit_attempt ON scalping_sl_state
             WHEN NEW.exit_attempt LIKE '%order_id%'
             BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
        )
        .unwrap();
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(
        placed.iter().map(|p| p.1.quantity).collect::<Vec<_>>(),
        vec![1755],
        "the second chunk is not sent while the first cannot be saved"
    );
    assert_eq!(m.mon.leg_count(), 1);
    // The sent order is still followed in memory.
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert_eq!(a.children.len(), 1);
    assert_eq!(m.alerts.lock().len(), 1);
}

/// P0-3: a Live exit with no definite answer that the order book does not
/// show is never released on absence alone. It stays unconfirmed (nothing
/// is sent again) until the trader marks it not placed; then the stop
/// exits again.
#[tokio::test]
async fn a_live_unconfirmed_exit_absent_from_the_book_is_held_for_the_trader() {
    use openalgo_desktop_lib::services::order_reconciler::{ABSENT_READS, ABSENT_SPAN_SECS};
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    *m.gw.uncertain.lock() = true;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    *m.gw.uncertain.lock() = false;
    let start = m.clock.now();
    for i in 0..(ABSENT_READS * 4) {
        let at = start + chrono::Duration::seconds(i64::from(i) * ABSENT_SPAN_SECS);
        assert_eq!(
            m.mon.reconcile_book(RunMode::Live, &book(vec![], at)).await,
            1
        );
    }
    let held = m.mon.held_orders();
    assert_eq!(held.len(), 1, "held for the trader");
    assert_eq!(held[0].action, "SELL");
    assert!(
        held[0].message.contains("not placed"),
        "{}",
        held[0].message
    );
    for s in 0..3 {
        m.clock.advance(chrono::Duration::seconds(4));
        m.mon.process_tick(OPT, "NFO", 93.0 - s as f64).await;
        m.mon.wait_idle().await;
    }
    assert_eq!(m.gw.placed.lock().len(), 1, "never sent again on absence");
    assert_eq!(m.mon.leg_count(), 1);
    // The trader checked the broker: not placed. The leg is exitable again.
    assert_eq!(m.mon.confirm_not_placed(&held[0].id).await, Ok(true));
    assert!(m.mon.held_orders().is_empty());
    assert_eq!(m.mon.confirm_not_placed(&held[0].id).await, Ok(false));
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 2);
}

#[tokio::test]
async fn ticks_flow_through_the_owned_task_and_stop_releases_everything() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.start();
    for _ in 0..200 {
        if !m.prices.held.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(m.prices.held.lock().len(), 1);
    assert_eq!(m.mon.task_count(), 1);
    // A tick on the source drives the exit.
    let tick = openalgo_desktop_lib::brokers::common::streaming::NormalizedTick {
        symbol: OPT.into(),
        exchange: "NFO".into(),
        ltp: 90.0,
        ..Default::default()
    };
    m.prices
        .tx
        .send(Arc::new(
            openalgo_desktop_lib::brokers::common::streaming::FeedEvent::Tick(tick),
        ))
        .unwrap();
    for _ in 0..200 {
        if !m.gw.placed.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);

    m.store.upsert_sl(&leg("analyze")).unwrap();
    m.mon.request_sync();
    // The live leg stays protected (its exit has not filled) beside the new
    // sandbox one.
    for _ in 0..200 {
        if m.mon.leg_count() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(m.prices.held.lock().len(), 1);
    m.mon.stop().await;
    assert!(!m.mon.is_running());
    assert_eq!(m.mon.task_count(), 0);
    assert!(
        m.prices.held.lock().is_empty(),
        "every subscription released"
    );
    assert!(m.mon.subscribed().is_empty());
    assert_eq!(m.mon.tracked_entries(), 0);
    // Start and stop many times: nothing accumulates.
    for _ in 0..50 {
        m.mon.start();
        m.mon.sync().await;
        m.mon.stop().await;
    }
    assert_eq!(m.mon.task_count(), 0);
    assert!(m.prices.held.lock().is_empty());
}

#[test]
fn exit_chunks_are_whole_lots() {
    assert_eq!(chunks(1950, Some(1755)), vec![1755, 195]);
    assert_eq!(chunks(65, Some(1755)), vec![65]);
    assert_eq!(chunks(10, None), vec![10]);
    assert_eq!(chunks(3510, Some(1755)), vec![1755, 1755]);
}

// ------------------------------------------------------------------ the real app

async fn wait_for<F: Fn() -> bool>(f: F) {
    for _ in 0..400 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn analyzer_toggle_mid_session_does_not_send_a_live_exit_to_the_sandbox() {
    let h = H::new().await;
    *h.mock.positions.lock() = Some(Ok(vec![pos(OPT, "NFO", "MIS", 65)]));
    h.ctx.scalping.store.upsert_sl(&leg("live")).unwrap();
    h.ctx.scalping.monitor.sync().await;
    h.analyze(true);
    h.ctx.scalping.monitor.process_tick(OPT, "NFO", 90.0).await;
    h.ctx.scalping.monitor.wait_idle().await;
    assert_eq!(
        placed(&h),
        vec![("SELL".to_string(), 65)],
        "the broker got the exit"
    );
    let sb = h.ctx.sandbox.orderbook().await.unwrap();
    let sb = serde_json::to_value(sb).unwrap();
    assert!(
        sb["data"]["orders"].as_array().is_none_or(|a| a.is_empty()),
        "{}",
        sb
    );
    // Accepted, not yet filled: the leg stays protected (LOG-05).
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn the_monitor_follows_the_broker_session() {
    let h = H::new().await;
    // persist() announced the session: the monitor runs.
    wait_for(|| h.ctx.scalping.monitor.is_running()).await;
    assert!(h.ctx.scalping.monitor.is_running());
    h.ctx.scalping.store.upsert_sl(&leg("live")).unwrap();
    h.ctx.scalping.monitor.request_sync();
    wait_for(|| h.ctx.scalping.monitor.leg_count() == 1).await;
    assert_eq!(h.ctx.scalping.monitor.subscribed().len(), 1);
    openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService::revoke(
        &h.ctx,
        openalgo_desktop_lib::events::SessionEndReason::Logout,
    )
    .await
    .unwrap();
    wait_for(|| !h.ctx.scalping.monitor.is_running()).await;
    wait_for(|| h.ctx.scalping.monitor.task_count() == 0).await;
    assert!(!h.ctx.scalping.monitor.is_running());
    assert_eq!(h.ctx.scalping.monitor.task_count(), 0);
    assert!(h.ctx.scalping.monitor.subscribed().is_empty());
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 0);
    assert_eq!(
        h.ctx.scalping.store.active_sl(None).unwrap().len(),
        1,
        "the stop is kept for the next session"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn post_sl_reaches_the_running_monitor() {
    let h = H::new().await;
    wait_for(|| h.ctx.scalping.monitor.is_running()).await;
    let (cookie, csrf) = session(&h);
    let body = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "side": "BUY", "quantity": 65, "entry_price": 100, "initial_sl": 95});
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/sl",
            Some(body),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    wait_for(|| h.ctx.scalping.monitor.leg_count() == 1).await;
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 1);
    let mon = h.ctx.scalping.monitor.clone();
    h.shutdown().await;
    assert_eq!(mon.task_count(), 0, "app shutdown stops the monitor");
    assert!(mon.subscribed().is_empty());
}

/// LOG-09: the side an entry goes to is decided once, before it is sent,
/// and the scalping list names that side. Read again after the order came
/// back, a toggle flipped while it was in flight tagged a live entry as a
/// sandbox one, so the live list (and Close All) lost it.
#[tokio::test]
async fn an_entry_is_tracked_on_the_side_it_was_sent_to() {
    let h = H::new().await;
    let (cookie, csrf) = session(&h);
    let gate = Arc::new(openalgo_desktop_lib::brokers::mock::PlaceGate::default());
    *h.mock.place_gate.lock() = Some(gate.clone());
    let body = json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "product": "NRML", "quantity": 130, "lots": 2});
    let flip = async {
        // The order is at the broker: the trader switches to analyzer mode.
        gate.entered.notified().await;
        h.analyze(true);
        gate.release.notify_one();
    };
    let ((s, b), ()) = tokio::join!(
        call(
            &h,
            req(
                Method::POST,
                "/scalping/api/order",
                Some(body),
                Some(&cookie),
                Some(&csrf),
            ),
        ),
        flip
    );
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(placed(&h), vec![("BUY".to_string(), 130)], "it went live");
    h.analyze(false);
    let (_, b) = call(
        &h,
        req(
            Method::GET,
            "/scalping/api/tracked",
            None,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(
        b["data"],
        json!([{"symbol": OPT, "exchange": "NFO", "product": "NRML", "mode": "live"}])
    );
}
