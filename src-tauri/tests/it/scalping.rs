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
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::db::sqlite::SqliteDb;
use openalgo_desktop_lib::events::subscribers::socketio::UiEmitter;
use openalgo_desktop_lib::scalping::monitor::{chunks, MonitorDeps, RiskMonitor};
use openalgo_desktop_lib::scalping::store::{SlUpsert, Store};
use openalgo_desktop_lib::strategy::dispatch::{
    Book, DispatchResult, OrderGateway, OrderPayload, OrderStatusResult, RunMode,
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
    delay_ms: Mutex<u64>,
    net: Mutex<i64>,
    placed: Mutex<Vec<(RunMode, OrderPayload)>>,
    books: Mutex<Vec<RunMode>>,
}

#[async_trait]
impl OrderGateway for FakeGateway {
    async fn place(&self, mode: RunMode, order: &OrderPayload) -> DispatchResult {
        let d = *self.delay_ms.lock();
        if d > 0 {
            tokio::time::sleep(Duration::from_millis(d)).await;
        }
        self.placed.lock().push((mode, order.clone()));
        if *self.refuse.lock() {
            return DispatchResult::refused("Broker rejected the order");
        }
        DispatchResult {
            ok: true,
            broker_order_id: Some("X1".into()),
            response: json!({}),
            error: None,
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
    gw: Arc<FakeGateway>,
    prices: Arc<FakePrices>,
    ui: Arc<Ui>,
    clock: Arc<ManualClock>,
    _dir: tempfile::TempDir,
}

fn monitor() -> M {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(SqliteDb::new(&dir.path().join("t.db")).unwrap());
    let store = Store::new(db);
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
    M {
        mon,
        store,
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

#[tokio::test]
async fn stop_loss_exits_once_and_clears_the_leg() {
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
    // Ticks after the exit find nothing to manage.
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
    assert_eq!(m.mon.leg_count(), 0);
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
    for _ in 0..200 {
        if m.mon.leg_count() == 1 {
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
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 0);
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
