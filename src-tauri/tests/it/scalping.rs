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
use openalgo_desktop_lib::scalping::monitor::{
    chunks, MonitorDeps, RiskMonitor, BOOK_LAG_GRACE_MS,
};
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

/// The whole-lot exit chunk the monitor uses for `OPT` (NIFTY, lot 65): the
/// exchange freeze rounded down to whole lots. The split tests take their
/// sizes from it, so a new NSE freeze table (as on 5 October 2026, web
/// #2218) does not break them.
fn freeze_chunk() -> i64 {
    let raw = openalgo_desktop_lib::services::freeze_qty_service::FreezeLimits::default()
        .for_option(OPT, "NFO");
    assert!(raw >= 65, "no freeze limit for {}", OPT);
    (raw / 65) * 65
}

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
    // 60 lots: above the NIFTY freeze from 2 November 2026 (1800 = 27 whole
    // lots of 65, so 1755 per order; NSE/FAOP/77189).
    *h.mock.positions.lock() = Some(Ok(vec![
        pos(OPT, "NFO", "NRML", 65 * 60),
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
        vec![
            ("SELL".to_string(), 1755),
            ("SELL".to_string(), 1755),
            ("SELL".to_string(), 390)
        ],
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

/// A scripted placement with no definite answer.
const UNCERTAIN: &str = "uncertain";

#[derive(Default)]
struct FakeGateway {
    refuse: Mutex<bool>,
    /// Placements answer with no definite outcome (LOG-08).
    uncertain: Mutex<bool>,
    /// Per placement, front first: `None` accepts, `Some(reason)` refuses,
    /// `Some(UNCERTAIN)` answers with no definite outcome.
    script: Mutex<std::collections::VecDeque<Option<&'static str>>>,
    /// While set, trade-book reads (and only those) wait on it.
    trades_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    delay_ms: Mutex<u64>,
    net: Mutex<i64>,
    /// The position book has no row for the leg (empty or lagging).
    empty: Mutex<bool>,
    placed: Mutex<Vec<(RunMode, OrderPayload)>>,
    books: Mutex<Vec<RunMode>>,
    /// While set, every position-book read waits on it (barrier).
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    entered: tokio::sync::Notify,
    /// A placement reached the broker (before any `delay_ms`): the exit
    /// it sends was written to the stop's row before it was sent.
    placing: tokio::sync::Notify,
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
        self.placing.notify_one();
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
        match self.script.lock().pop_front() {
            Some(Some(UNCERTAIN)) => {
                return DispatchResult {
                    ok: false,
                    error: Some("The broker did not confirm whether this order was placed.".into()),
                    uncertain: true,
                    ..Default::default()
                }
            }
            Some(Some(reason)) => return DispatchResult::refused(reason),
            _ => {}
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
    async fn book(&self, mode: RunMode, which: Book) -> Result<Value, Value> {
        if which == Book::Trades {
            let gate = self.trades_gate.lock().clone();
            if let Some(g) = gate {
                self.entered.notify_one();
                g.notified().await;
            }
            return Ok(json!({"status": "success", "data": []}));
        }
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
    /// Ticks the monitor said it skipped (NEW-REL-05).
    skipped: std::sync::atomic::AtomicU64,
}

impl FakePrices {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tx: broadcast::channel(64).0,
            held: Mutex::new(vec![]),
            skipped: std::sync::atomic::AtomicU64::new(0),
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
    fn note_skipped(&self, n: u64) {
        self.skipped
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
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
        freeze: Default::default(),
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
        "average_price": 94.5, "timestamp": "2026-10-05 10:30:05",
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
    let f = freeze_chunk();
    let m = monitor();
    *m.gw.net.lock() = f + 195;
    let mut l = leg("live");
    l.quantity = Some(f + 195);
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
        vec![f, 195]
    );
    // A delayed partial fill of the first chunk, and the book follows it.
    *m.gw.net.lock() = f + 195 - 1000;
    m.mon
        .reconcile_book(
            RunMode::Live,
            &book(
                vec![json!({
                    "orderid": "X1", "symbol": OPT, "exchange": "NFO", "action": "SELL",
                    "quantity": f, "product": "MIS", "order_status": "open",
                    "filled_quantity": 1000, "average_price": 94.5, "timestamp": "2026-10-05 10:30:05",
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
        "the position less 1000 filled less the rest still working"
    );
    // Total exit quantity sent never exceeds the position.
    let accepted: i64 = [placed[0].1.quantity, placed[2].1.quantity].iter().sum();
    assert_eq!(accepted, f + 195);
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
                    order_row("X1", "SELL", f, "complete"),
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
    // Saved and pushed by a task of its own, never awaited by the tick.
    m.mon.wait_idle().await;
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
    m.mon.wait_idle().await;
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
    assert_eq!(
        m.mon.leg_count(),
        1,
        "one zero read is not proof (review P1-4)"
    );
    // Zero for as long as the book can lag an entry: flat.
    m.clock
        .advance(chrono::Duration::milliseconds(BOOK_LAG_GRACE_MS + 1000));
    m.mon.process_tick(OPT, "NFO", 89.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty());
    assert_eq!(m.mon.leg_count(), 0);
}

/// Review P1-4: right after an entry the position book can still show an
/// earlier, closed row of the same contract at zero. A breach then sends
/// nothing and keeps the stop; when the book catches up, the exit goes out.
#[tokio::test]
async fn a_stale_zero_row_right_after_entry_keeps_protection() {
    let m = monitor();
    *m.gw.net.lock() = 0;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.mon.leg_count(), 1, "not cleared on a stale zero");
    assert!(!cleared(&m));
    // A few seconds on, the book shows the new position.
    *m.gw.net.lock() = 65;
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 89.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].1.quantity, 65);
}

/// Review P2-5: an exit order is written before it is sent, so quitting
/// while it is in flight leaves it unconfirmed on the stop's row. After a
/// restart it is looked up in the book, never sent again blindly.
#[tokio::test]
async fn quitting_while_an_exit_is_in_flight_never_sends_it_again() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    *m.gw.delay_ms.lock() = 5_000;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    // The exit is in flight at the broker when the app quits. Waited for
    // as the event, not a timer: 200 sleeps of 5 ms ended early on
    // Windows' coarse timers and the app quit before the exit was sent.
    tokio::time::timeout(Duration::from_secs(60), m.gw.placing.notified())
        .await
        .expect("the exit reached the broker");
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS");
    assert!(
        a.as_ref().is_some_and(|a| a.children.len() == 1),
        "written before it was sent: {:?}",
        a
    );
    m.mon.stop().await;
    *m.gw.delay_ms.lock() = 0;
    // Started again from the rows.
    m.mon.sync().await;
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert_eq!(a.children.len(), 1, "{:?}", a);
    assert_eq!(a.children[0].qty, 65);
    m.clock.advance(chrono::Duration::seconds(10));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert!(
        m.gw.placed.lock().is_empty(),
        "never sent again while its outcome is unknown"
    );
}

/// Review P1-5: an exit the broker accepted that its order book no longer
/// shows is never left working silently: in Live it is held for the trader
/// and alerted, and marking it not placed lets the stop exit again.
#[tokio::test]
async fn an_accepted_exit_the_book_no_longer_shows_is_held_for_the_trader() {
    use openalgo_desktop_lib::services::order_reconciler::{ABSENT_READS, ABSENT_SPAN_SECS};
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    let start = m.clock.now();
    for i in 0..=ABSENT_READS {
        let at = start + chrono::Duration::seconds(i64::from(i) * ABSENT_SPAN_SECS);
        m.mon.reconcile_book(RunMode::Live, &book(vec![], at)).await;
    }
    let held = m.mon.held_orders();
    assert_eq!(held.len(), 1, "held for the trader");
    assert!(held[0].message.contains("X1"), "{}", held[0].message);
    assert_eq!(m.mon.confirm_not_placed(&held[0].id).await, Ok(true));
    m.clock.advance(chrono::Duration::seconds(10));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 2, "the stop exits again");
}

/// Review P1-5: exit orders from an earlier trading session cannot still
/// be working. On start they are ended, and the stop protects the position
/// again instead of waiting on them forever.
#[tokio::test]
async fn exit_orders_from_an_earlier_session_are_ended_on_start() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    // The app is closed overnight; the order expired with the session.
    m.mon.stop().await;
    m.clock.advance(chrono::Duration::days(1));
    m.mon.sync().await;
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert!(a.children.iter().all(|c| c.terminal()), "{:?}", a);
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 2, "protected again");
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
    let f = freeze_chunk();
    let m = monitor();
    *m.gw.net.lock() = f + 195;
    let mut l = leg("live");
    l.quantity = Some(f + 195);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    // The empty attempt saves; one that records a sent order does not.
    m.db.conn()
        .unwrap()
        .execute_batch(
            r#"CREATE TRIGGER no_child BEFORE UPDATE OF exit_attempt ON scalping_sl_state
             WHEN NEW.exit_attempt LIKE '%"order_id":"%'
             BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;"#,
        )
        .unwrap();
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(
        placed.iter().map(|p| p.1.quantity).collect::<Vec<_>>(),
        vec![f],
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

/// LOG-08 review: a reconcile pass that waits on the broker (reading the
/// trade book for a held exit) while an exit sends more orders keeps those
/// orders. The pass folds its facts into the attempt as it stands; it does
/// not write back its older copy, which made the next breach send again.
#[tokio::test]
async fn a_reconcile_pass_never_drops_orders_an_exit_sent_meanwhile() {
    use openalgo_desktop_lib::scalping::monitor::ScalpingOrders;
    use openalgo_desktop_lib::services::order_reconciler::{
        OrderOwner, ABSENT_READS, ABSENT_SPAN_SECS, LIVE_RELEASE_SECS,
    };
    let f = freeze_chunk();
    let m = monitor();
    *m.gw.net.lock() = 2 * f + 195;
    let mut l = leg("live");
    l.quantity = Some(2 * f + 195);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    // One chunk unconfirmed, one accepted (X2), 195 refused.
    {
        let mut s = m.gw.script.lock();
        s.push_back(Some(UNCERTAIN));
        s.push_back(None);
        s.push_back(Some("Exchange busy"));
    }
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert_eq!(
        m.gw.placed
            .lock()
            .iter()
            .map(|p| p.1.quantity)
            .collect::<Vec<_>>(),
        vec![f, f, 195]
    );
    // The unconfirmed order's absence is confirmed: held (Live).
    let start = m.clock.now();
    for i in 0..=ABSENT_READS {
        let at = start + chrono::Duration::seconds(i64::from(i) * ABSENT_SPAN_SECS);
        m.mon.reconcile_book(RunMode::Live, &book(vec![], at)).await;
    }
    // Ten minutes on, a pass reads the trade book for it (the automatic
    // release check) and waits at the broker, while X2 is partly filled.
    let gate = Arc::new(tokio::sync::Notify::new());
    *m.gw.trades_gate.lock() = Some(gate.clone());
    let owners: Vec<Arc<dyn OrderOwner>> = vec![Arc::new(ScalpingOrders(Arc::downgrade(&m.mon)))];
    let partly = book(
        vec![json!({
            "orderid": "X2", "symbol": OPT, "exchange": "NFO", "action": "SELL",
            "quantity": f, "product": "MIS", "order_status": "open",
            "filled_quantity": 1000, "average_price": 94.5, "timestamp": "2026-10-05 10:30:05",
        })],
        start + chrono::Duration::seconds(LIVE_RELEASE_SECS + 1),
    )
    .with_source(m.gw.clone(), RunMode::Live)
    .owned_by(&owners)
    .await;
    let mon = m.mon.clone();
    let pass = tokio::spawn(async move { mon.reconcile_book(RunMode::Live, &partly).await });
    m.gw.entered.notified().await;
    // Meanwhile the stop breaches again: the refused 195 goes out (X4).
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 89.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 4);
    // X2's 1000 shows in the position: the unconfirmed order is not
    // released (the position is not what it was before sending).
    *m.gw.net.lock() = 2 * f + 195 - 1000;
    *m.gw.trades_gate.lock() = None;
    gate.notify_one();
    pass.await.unwrap();
    let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
    assert_eq!(
        a.children.len(),
        3,
        "the order sent meanwhile is kept: {:?}",
        a
    );
    assert_eq!(a.children[1].filled, 1000, "and the pass's fill is folded");
    // Everything is covered: the next breach sends nothing.
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 88.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 4, "never a second 195");
}

/// LOG-08 review: when several book orders match an unconfirmed scalping
/// exit and nothing tells them apart, the trader is alerted, once (it was
/// only logged).
#[tokio::test]
async fn an_ambiguous_unconfirmed_exit_alerts_the_trader_once() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    *m.gw.uncertain.lock() = true;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    *m.gw.uncertain.lock() = false;
    let rows = vec![
        order_row("B1", "SELL", 65, "open"),
        order_row("B2", "SELL", 65, "open"),
    ];
    for s in 0..2 {
        let at = m.clock.now() + chrono::Duration::seconds(s);
        m.mon
            .reconcile_book(RunMode::Live, &book(rows.clone(), at))
            .await;
    }
    let alerts = m.alerts.lock().clone();
    assert_eq!(
        alerts
            .iter()
            .filter(|a| a.contains("nothing tells them apart"))
            .count(),
        1,
        "{:?}",
        alerts
    );
    assert_eq!(m.gw.placed.lock().len(), 1, "never sent again");
}

/// LOG-08 review: a stop the trader removes takes its held exits with it,
/// so the Health page does not keep offering them.
#[tokio::test]
async fn a_removed_stop_takes_its_held_exits_with_it() {
    use openalgo_desktop_lib::services::order_reconciler::{ABSENT_READS, ABSENT_SPAN_SECS};
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    *m.gw.uncertain.lock() = true;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    *m.gw.uncertain.lock() = false;
    let start = m.clock.now();
    for i in 0..=ABSENT_READS {
        let at = start + chrono::Duration::seconds(i64::from(i) * ABSENT_SPAN_SECS);
        m.mon.reconcile_book(RunMode::Live, &book(vec![], at)).await;
    }
    assert_eq!(m.mon.held_orders().len(), 1);
    m.store.delete_sl(OPT, "NFO", "MIS", Some("live")).unwrap();
    m.mon.sync().await;
    assert!(m.mon.held_orders().is_empty());
}

/// NEW-REL-05: ticks the monitor's reader skipped because it fell behind
/// the feed are reported to its price source, which counts them against the
/// broker feed for health. They were a debug log line only.
#[tokio::test]
async fn ticks_the_monitor_skips_are_counted_against_the_feed() {
    let m = monitor();
    m.mon.start();
    // The monitor's task has not run yet on this test's one thread, so 200
    // ticks into the 64-tick channel leave its reader 136 behind.
    for i in 0..200 {
        let tick = openalgo_desktop_lib::brokers::common::streaming::NormalizedTick {
            symbol: OPT.into(),
            exchange: "NFO".into(),
            ltp: 100.0 + i as f64 * 0.05,
            ..Default::default()
        };
        let _ = m.prices.tx.send(Arc::new(
            openalgo_desktop_lib::brokers::common::streaming::FeedEvent::Tick(tick),
        ));
    }
    let skipped = || m.prices.skipped.load(std::sync::atomic::Ordering::SeqCst);
    for _ in 0..200 {
        if skipped() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(skipped(), 136);
    m.mon.stop().await;
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
    assert_eq!(chunks(1950, Some(1755)), Ok(vec![1755, 195]));
    assert_eq!(chunks(65, Some(1755)), Ok(vec![65]));
    assert_eq!(chunks(10, None), Ok(vec![10]));
    assert_eq!(chunks(3510, Some(1755)), Ok(vec![1755, 1755]));
    let full = chunks(130_000, Some(1300)).unwrap();
    assert_eq!((full.len(), full.iter().sum::<i64>()), (100, 130_000));
}

/// Count before allocating: an exit's orders are counted before any is
/// built, and more than 100 is refused. The quantity is the broker's
/// position book's; one of 2147483647 in orders of 1 asked for 16 GiB
/// before anything was counted (the cap makes that abort, not swap).
#[test]
fn an_exit_counts_its_orders_before_building_them() {
    let (refused, largest) = crate::alloc_support::capped_sync(64 << 20, || {
        [
            chunks(i64::from(i32::MAX), Some(1)),
            chunks(i64::MAX, Some(1)),
            chunks(130_065, Some(1300)),
        ]
    });
    assert!(refused.iter().all(Result::is_err), "{:?}", refused);
    assert!(largest < 4096, "{} bytes in one allocation", largest);
    assert_eq!(
        refused[2],
        Err("Closing 130065 would take more than 100 orders of 1300, so no exit was sent".into())
    );
}

/// An exit that would take more than 100 orders (a position book showing
/// far more than the stop was set for) is refused like any exit that cannot
/// go out: nothing is sent, the stop stays managed, and the trader is told
/// why. Before, all 101 orders went out.
#[tokio::test]
async fn an_exit_needing_more_than_100_orders_is_refused_and_the_stop_stays() {
    let m = monitor();
    // 100 orders of the freeze-sized chunk, and one lot more.
    let net = freeze_chunk() * 100 + 65;
    *m.gw.net.lock() = net;
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 94.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty(), "no exit order was sent");
    assert_eq!(m.mon.leg_count(), 1, "the stop stays managed");
    assert_eq!(m.store.active_sl(None).unwrap().len(), 1);
    let alerts = m.alerts.lock().clone();
    assert_eq!(alerts.len(), 1, "{:?}", alerts);
    let why = format!(
        "Closing {} would take more than 100 orders of {}",
        net,
        freeze_chunk()
    );
    assert!(alerts[0].contains(&why), "{}", alerts[0]);
    // Its next breach is refused again, and told once.
    m.clock.advance(chrono::Duration::seconds(4));
    m.mon.process_tick(OPT, "NFO", 93.0).await;
    m.mon.wait_idle().await;
    assert!(m.gw.placed.lock().is_empty());
    assert_eq!(m.alerts.lock().len(), 1);
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

/// The scalping page's MARKET entry body for the test contract.
fn page_entry() -> Value {
    json!({"symbol": OPT, "exchange": "NFO", "action": "BUY", "product": "NRML", "quantity": 65, "lots": 1})
}

/// LOG-08: an entry from the scalping page reaches the broker and its answer
/// is lost. The page got the raw error and a click to retry sent it again.
/// Now the trader reads that the broker did not confirm it, a retry (and an
/// exit for that contract) is refused while it is unconfirmed, and once the
/// order book shows it the entry joins the scalping list and the page trades
/// again.
#[tokio::test]
async fn an_unconfirmed_entry_is_not_sent_again_until_the_book_settles_it() {
    let h = H::new().await;
    // Book reads are driven by hand.
    h.ctx.reconciler.stop();
    let (cookie, csrf) = session(&h);
    h.mock.after_send.lock().push_back((
        openalgo_desktop_lib::brokers::mock::AfterSend::Dropped,
        "complete",
    ));
    *h.mock.after_send_time.lock() = "2026-10-03 09:42:05".into();
    let order = |b: &Value| {
        req(
            Method::POST,
            "/scalping/api/order",
            Some(b.clone()),
            Some(&cookie),
            Some(&csrf),
        )
    };
    let (s, b) = call(&h, order(&page_entry())).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{}", b);
    assert_eq!(
        b["message"],
        openalgo_desktop_lib::scalping::unconfirmed::NOT_CONFIRMED
    );
    assert_eq!(placed(&h).len(), 1);

    // The click to retry, and an exit for that contract, are refused.
    let (s, b) = call(&h, order(&page_entry())).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", b);
    assert!(b["message"].as_str().unwrap().contains("was not confirmed"));
    let (s, _) = call(
        &h,
        req(
            Method::POST,
            "/scalping/api/close_leg",
            Some(json!({"symbol": OPT, "exchange": "NFO", "action": "SELL", "product": "NRML", "quantity": 65})),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(placed(&h).len(), 1, "nothing more reached the broker");

    // The order book settles it: tracked, and the page trades again.
    h.ctx.reconciler.pass().await;
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
    let (s, b) = call(&h, order(&page_entry())).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(placed(&h).len(), 2);
}

/// LOG-08 across a restart: an order from the scalping page that the broker
/// did not confirm is kept in the database. After the app reopens, a repeat
/// for that contract is still refused, and once the broker's order book shows
/// the order it joins the scalping list and the page trades again.
#[tokio::test]
async fn an_unconfirmed_scalping_order_survives_a_restart() {
    use openalgo_desktop_lib::scalping::unconfirmed::{record, NewPending};
    // The app stopped with an unconfirmed live entry on record.
    let h = H::new_with(|dir| {
        let db = SqliteDb::new(&dir.join("openalgo.db")).unwrap();
        record(
            &db,
            &NewPending {
                mode: "live",
                kind: "entry",
                symbol: OPT,
                exchange: "NFO",
                product: "NRML",
                action: "BUY",
                quantity: 65,
                client_tag: None,
            },
            crate::api_v1_support::session_time(),
        )
        .unwrap();
    })
    .await;
    h.ctx.reconciler.stop();
    let (cookie, csrf) = session(&h);
    let order = |b: &Value| {
        req(
            Method::POST,
            "/scalping/api/order",
            Some(b.clone()),
            Some(&cookie),
            Some(&csrf),
        )
    };
    let (s, b) = call(&h, order(&page_entry())).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", b);
    assert!(b["message"].as_str().unwrap().contains("was not confirmed"));
    assert!(placed(&h).is_empty(), "nothing reached the broker");

    // The broker's book holds it: the reconciler settles it.
    *h.mock.order_book.lock() = Some(Ok(vec![openalgo_desktop_lib::brokers::types::Order {
        order_id: "BRK-77".into(),
        exchange_order_id: None,
        symbol: OPT.into(),
        exchange: "NFO".into(),
        side: "BUY".into(),
        quantity: 65,
        filled_quantity: 65,
        pending_quantity: 0,
        price: 0.0,
        trigger_price: 0.0,
        average_price: 120.0,
        order_type: "MARKET".into(),
        product: "NRML".into(),
        status: "complete".into(),
        validity: "DAY".into(),
        order_timestamp: "2026-10-03 09:42:05".into(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }]));
    h.ctx.reconciler.pass().await;
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
    let (s, b) = call(&h, order(&page_entry())).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(placed(&h).len(), 1);
}

/// LOG-08 P0-3 on the scalping page: a live entry whose answer was lost and
/// that the broker's book never shows is never released on absence alone,
/// however long. It is listed under Unconfirmed Orders on the Health page
/// with a critical alert, the contract stays refused, and the trader's "not
/// placed" there releases it.
#[tokio::test]
async fn a_held_page_order_is_released_only_by_the_traders_not_placed() {
    use openalgo_desktop_lib::services::order_reconciler::{
        ABSENT_READS, ABSENT_SPAN_SECS, LIVE_RELEASE_SECS,
    };
    let h = H::new().await;
    h.ctx.reconciler.stop();
    let (cookie, csrf) = session(&h);
    let get = |path: &str| req(Method::GET, path, None, Some(&cookie), None);
    let order = || {
        req(
            Method::POST,
            "/scalping/api/order",
            Some(page_entry()),
            Some(&cookie),
            Some(&csrf),
        )
    };
    h.mock.after_send.lock().push_back((
        openalgo_desktop_lib::brokers::mock::AfterSend::Dropped,
        "open",
    ));
    let (s, _) = call(&h, order()).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    // The broker's book never shows it.
    *h.mock.order_book.lock() = Some(Ok(Vec::new()));
    for _ in 0..=ABSENT_READS {
        h.clock.advance(chrono::Duration::seconds(ABSENT_SPAN_SECS));
        h.ctx.reconciler.pass().await;
    }
    let (_, body) = call(&h, get("/health/api/held-orders")).await;
    let rows = body["data"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "{}", body);
    assert_eq!(rows[0]["owner"], "scalping-page");
    assert_eq!(rows[0]["action"], "BUY");
    assert_eq!(rows[0]["quantity"], 65);
    assert!(rows[0]["message"]
        .as_str()
        .unwrap()
        .starts_with("Scalping page"));
    let (_, alerts) = call(&h, get("/health/api/alerts")).await;
    assert!(
        alerts.to_string().contains("order_unconfirmed_held"),
        "{}",
        alerts
    );
    // Long past the release window: still held, still refused.
    h.clock
        .advance(chrono::Duration::seconds(LIVE_RELEASE_SECS + 60));
    h.ctx.reconciler.pass().await;
    let (s, _) = call(&h, order()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(placed(&h).len(), 1);

    // The trader checked the books and marks it not placed.
    let confirm = json!({"owner": "scalping-page", "id": rows[0]["id"]});
    let (s, b) = call(
        &h,
        req(
            Method::POST,
            "/health/api/held-orders/not-placed",
            Some(confirm.clone()),
            Some(&cookie),
            Some(&csrf),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let (_, body) = call(&h, get("/health/api/held-orders")).await;
    assert!(body["data"].as_array().unwrap().is_empty(), "{}", body);
    let (s, b) = call(&h, order()).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    assert_eq!(placed(&h).len(), 2);
}

/// LOG-08 with a freeze-split exit from the scalping page: the first
/// chunk's answer is lost. The split sends nothing after it, the
/// chunk is recorded as unconfirmed with its own quantity, and the order
/// book settles it.
#[tokio::test]
async fn a_split_exit_records_the_chunk_the_broker_did_not_confirm() {
    let h = H::new().await;
    h.ctx.reconciler.stop();
    let (cookie, csrf) = session(&h);
    h.mock.after_send.lock().push_back((
        openalgo_desktop_lib::brokers::mock::AfterSend::Dropped,
        "open",
    ));
    *h.mock.after_send_time.lock() = "2026-10-03 09:42:05".into();
    // 60 lots: above the NIFTY freeze (1,800 from 2 November 2026, 3,510
    // from 5 October), so the exit goes out in at least two freeze-sized chunks.
    let close = || {
        req(
            Method::POST,
            "/scalping/api/close_leg",
            Some(
                json!({"symbol": OPT, "exchange": "NFO", "action": "SELL", "product": "NRML", "quantity": 65 * 60}),
            ),
            Some(&cookie),
            Some(&csrf),
        )
    };
    let (s, b) = call(&h, close()).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{}", b);
    assert_eq!(
        b["message"],
        openalgo_desktop_lib::scalping::unconfirmed::NOT_CONFIRMED
    );
    let sent = placed(&h);
    assert_eq!(sent.len(), 1, "nothing after the first chunk: {:?}", sent);
    let (action, first) = sent[0].clone();
    assert_eq!(action, "SELL");
    assert!(
        first < 65 * 60 && first % 65 == 0,
        "a freeze-sized chunk: {}",
        first
    );
    let pending =
        openalgo_desktop_lib::scalping::unconfirmed::pending(&h.ctx.sqlite, "live").unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (pending[0].kind.as_str(), pending[0].quantity),
        ("exit", first)
    );
    let (s, _) = call(&h, close()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(placed(&h).len(), 1, "nothing more reached the broker");

    // The book shows the chunk: settled, and the page can exit again.
    h.ctx.reconciler.pass().await;
    assert!(
        openalgo_desktop_lib::scalping::unconfirmed::pending(&h.ctx.sqlite, "live")
            .unwrap()
            .is_empty()
    );
    let (s, b) = call(&h, close()).await;
    assert_eq!(s, StatusCode::OK, "{}", b);
}

/// DB-01: a tick that moves a trailing stop saves it off the async
/// workers. While another writer holds the database for 400 ms, the
/// runtime is not held, and the stop is saved once the database is free.
#[tokio::test]
async fn a_trailing_stop_save_never_holds_the_runtime() {
    let m = monitor();
    let mut l = leg("analyze");
    l.trailing_enabled = Some(true);
    l.trailing_step = Some(5.0);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    let writer = m.db.conn().unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        writer.execute_batch("COMMIT").unwrap();
    });
    let ((), stall) = crate::runtime_stall(m.mon.process_tick(OPT, "NFO", 110.0)).await;
    release.join().unwrap();
    m.mon.wait_idle().await;
    assert!(
        stall < Duration::from_millis(150),
        "the runtime was held {:?}",
        stall
    );
    assert_eq!(m.store.active_sl(None).unwrap()[0].current_sl, Some(105.0));
}

/// Every connection of `db`'s pool held on another thread for `ms`; the
/// thread, to join. Returns once they are all held.
fn hold_the_pool(db: &Arc<SqliteDb>, ms: u64) -> std::thread::JoinHandle<()> {
    let db = db.clone();
    let (held, ready) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let conns: Vec<_> = (0..8).map(|_| db.conn().unwrap()).collect();
        held.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(ms));
        drop(conns);
    });
    ready.recv().unwrap();
    holder
}

/// Run `fut` to its end on a runtime of its own, on another thread, never
/// yielding this test's runtime: its tasks are not polled meanwhile.
fn elsewhere<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> T {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    })
    .join()
    .unwrap()
}

/// Wait on this thread, never yielding to the runtime, until `f` holds.
fn wait_here(f: impl Fn() -> bool) {
    for _ in 0..5000 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the condition never held");
}

/// The database's write lock held by another connection for `ms` (a long
/// import, a report's write); the thread, to join. Reads go on meanwhile.
fn hold_the_writes(db: &Arc<SqliteDb>, ms: u64) -> std::thread::JoinHandle<()> {
    let writer = db.conn().unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        writer.execute_batch("COMMIT").unwrap();
    })
}

/// Join a holder thread without holding the runtime.
async fn join(holder: std::thread::JoinHandle<()>) {
    tokio::task::spawn_blocking(move || holder.join().unwrap())
        .await
        .unwrap();
}

/// Poll `f` until it holds or `limit` has passed; whether it held.
async fn within(limit: Duration, f: impl Fn() -> bool) -> bool {
    let end = std::time::Instant::now() + limit;
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f()
}

/// A tick for `OPT` on the monitor's price source, as the feed sends it.
fn send_tick(m: &M, ltp: f64) {
    let tick = openalgo_desktop_lib::brokers::common::streaming::NormalizedTick {
        symbol: OPT.into(),
        exchange: "NFO".into(),
        ltp,
        ..Default::default()
    };
    m.prices
        .tx
        .send(Arc::new(
            openalgo_desktop_lib::brokers::common::streaming::FeedEvent::Tick(tick),
        ))
        .unwrap();
}

/// PROD-REL-02 follow-up: a sync runs beside the ticks. The consumer ran it
/// inside its own loop, so while its read of the stops waited for a pooled
/// connection (here every one is taken, as by long reports) no tick was
/// judged and the bounded tick channel fell behind. Now a breach on a held
/// leg, sent through the price source while the sync waits, is judged at
/// once (its exit reads the position book), and the runtime is never held;
/// once the database answers, the new stop is installed and exactly one exit
/// goes out, to the leg's own side.
#[test]
fn a_tick_is_judged_while_a_sync_waits_for_the_database() {
    crate::on_watched_runtime(|runtime| async move {
        let m = monitor();
        m.store.upsert_sl(&leg("live")).unwrap();
        m.mon.start();
        let watched = within(Duration::from_secs(2), || !m.prices.held.lock().is_empty()).await;
        assert!(watched, "the live stop is watched");
        let mut sandbox = leg("analyze");
        sandbox.initial_sl = Some(90.0);
        m.store.upsert_sl(&sandbox).unwrap();
        let reads = m.mon.stop_reads();
        let holder = hold_the_pool(&m.db, 1500);
        let (judged, hold) = runtime
            .longest_hold(async {
                m.mon.request_sync();
                // The consumer starts the sync; its read waits for a connection.
                tokio::time::sleep(Duration::from_millis(50)).await;
                send_tick(&m, 94.0);
                let read = within(Duration::from_millis(1000), || {
                    m.gw.books.lock().contains(&RunMode::Live)
                })
                .await;
                read && m.mon.stop_reads() == reads
            })
            .await;
        join(holder).await;
        assert!(
            hold < Duration::from_millis(200),
            "the runtime was held for {:?}",
            hold
        );
        assert!(judged, "the breach was judged while the sync waited");
        let installed = within(Duration::from_secs(3), || m.mon.leg_count() == 2).await;
        assert!(
            installed,
            "the new stop is installed once the database answers"
        );
        let sent = within(Duration::from_secs(3), || !m.gw.placed.lock().is_empty()).await;
        assert!(sent);
        m.mon.wait_idle().await;
        let placed = m.gw.placed.lock().clone();
        assert_eq!(placed.len(), 1, "{:?}", placed);
        assert_eq!(
            (
                placed[0].0,
                placed[0].1.action.as_str(),
                placed[0].1.quantity
            ),
            (RunMode::Live, "SELL", 65)
        );
        m.mon.stop().await;
    })
}

/// PROD-REL-02 follow-up: a trailed stop's save never holds the ticks
/// back. The tick that moved the stop awaited its save, so while another
/// writer held the database the consumer judged nothing. Now the pull-back
/// through the moved stop, sent right after, is judged at once (its exit
/// reads the position book), and the stop is saved once the database is
/// free.
#[tokio::test]
async fn a_tick_is_judged_while_a_trailed_stop_waits_to_be_saved() {
    let m = monitor();
    let mut l = leg("analyze");
    l.trailing_enabled = Some(true);
    l.trailing_step = Some(5.0);
    m.store.upsert_sl(&l).unwrap();
    m.mon.start();
    let watched = within(Duration::from_secs(2), || !m.prices.held.lock().is_empty()).await;
    assert!(watched, "the stop is watched");
    let writer = hold_the_writes(&m.db, 1500);
    // The stop trails to 105 and its save waits; then the price goes
    // through it.
    send_tick(&m, 110.0);
    send_tick(&m, 104.0);
    let judged = within(Duration::from_millis(1000), || {
        m.gw.books.lock().contains(&RunMode::Sandbox)
    })
    .await;
    let unsaved = m.store.active_sl(None).unwrap()[0].current_sl != Some(105.0);
    join(writer).await;
    assert_eq!(
        (judged, unsaved),
        (true, true),
        "(the breach judged, while the trail was not yet saved)"
    );
    let sent = within(Duration::from_secs(3), || !m.gw.placed.lock().is_empty()).await;
    assert!(sent);
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1, "{:?}", placed);
    assert_eq!(
        (
            placed[0].0,
            placed[0].1.action.as_str(),
            placed[0].1.quantity
        ),
        (RunMode::Sandbox, "SELL", 65)
    );
    assert_eq!(m.store.active_sl(None).unwrap()[0].current_sl, Some(105.0));
    m.mon.stop().await;
}

/// PROD-REL-02 follow-up: a sync that cannot read the saved stops is tried
/// again, with a capped backoff, and after three failures in a row the
/// trader is told that a stop saved meanwhile may not be watched. It gave up
/// at once and said nothing, so a stop saved while the stops could not be
/// read was never watched. Readable again, the stop is watched without being
/// saved again, and its breach exits.
#[tokio::test]
async fn a_sync_that_cannot_read_the_stops_tries_again_and_tells_the_trader() {
    use openalgo_desktop_lib::scalping::monitor::STOPS_UNREAD;
    let m = monitor();
    let rename = |from: &str, to: &str| {
        m.db.conn()
            .unwrap()
            .execute_batch(&format!("ALTER TABLE {} RENAME TO {}", from, to))
            .unwrap()
    };
    m.store.upsert_sl(&leg("live")).unwrap();
    rename("scalping_sl_state", "scalping_sl_state_unreadable");
    m.mon.start();
    let told = within(Duration::from_secs(5), || !m.alerts.lock().is_empty()).await;
    assert!(told, "the trader is told");
    assert_eq!(m.alerts.lock().clone(), vec![STOPS_UNREAD.to_string()]);
    assert!(m.mon.stop_reads() >= 3, "{} reads", m.mon.stop_reads());
    assert_eq!(m.mon.leg_count(), 0);

    rename("scalping_sl_state_unreadable", "scalping_sl_state");
    let watched = within(Duration::from_secs(10), || {
        m.mon.leg_count() == 1 && !m.prices.held.lock().is_empty()
    })
    .await;
    assert!(watched, "the stop is watched without being saved again");
    send_tick(&m, 94.0);
    let sent = within(Duration::from_secs(3), || !m.gw.placed.lock().is_empty()).await;
    assert!(sent, "its breach exits");
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    assert_eq!(m.alerts.lock().len(), 1, "told once");
    m.mon.stop().await;
}

/// PROD-REL-02 follow-up: an exit's order is saved before it is sent, and
/// that save no longer holds the monitor's lock. It did, so while another
/// writer held the database (here for 800 ms) the save of one leg's exit
/// held the runtime and every other leg: a breach on another leg was not
/// judged until it ended. Now the other leg's breach is judged meanwhile
/// (its exit reads the position book before the first order is sent), and
/// both exits go out once the database is free, each to its own side.
#[test]
fn another_legs_breach_is_judged_while_an_exit_waits_to_be_saved() {
    crate::on_watched_runtime(|runtime| async move {
        let m = monitor();
        m.store.upsert_sl(&leg("live")).unwrap();
        let mut sandbox = leg("analyze");
        sandbox.initial_sl = Some(90.0);
        m.store.upsert_sl(&sandbox).unwrap();
        m.mon.sync().await;
        // Both legs' high and low noted and saved now; the clock stands, so
        // no later move is saved again (PERSIST_THROTTLE_MS): the exits'
        // saves are the only writes that wait below.
        m.mon.process_tick(OPT, "NFO", 96.0).await;
        m.mon.wait_idle().await;
        let writer = hold_the_writes(&m.db, 800);
        let (judged, hold) = runtime
            .longest_hold(async {
                m.mon.process_tick(OPT, "NFO", 94.0).await;
                // The live leg's exit reads its position, then waits to
                // save its order before sending it.
                within(Duration::from_millis(500), || {
                    m.gw.books.lock().contains(&RunMode::Live)
                })
                .await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                m.mon.process_tick(OPT, "NFO", 89.0).await;
                let read = within(Duration::from_millis(500), || {
                    m.gw.books.lock().contains(&RunMode::Sandbox)
                })
                .await;
                (read, m.gw.placed.lock().len())
            })
            .await;
        join(writer).await;
        assert!(
            hold < Duration::from_millis(200),
            "the runtime was held for {:?}",
            hold
        );
        assert_eq!(
            judged,
            (true, 0),
            "the other leg was judged (its position read) while the exit waited to be saved"
        );
        let sent = within(Duration::from_secs(3), || m.gw.placed.lock().len() == 2).await;
        assert!(sent, "{:?}", m.gw.placed.lock());
        m.mon.wait_idle().await;
        let placed: Vec<_> =
            m.gw.placed
                .lock()
                .iter()
                .map(|(mode, o)| (*mode, o.action.clone(), o.quantity))
                .collect();
        assert_eq!(placed.len(), 2, "{:?}", placed);
        for side in [RunMode::Live, RunMode::Sandbox] {
            assert!(
                placed.contains(&(side, "SELL".to_string(), 65)),
                "{:?}",
                placed
            );
        }
    })
}

/// PROD-REL-02 follow-up: exit orders of an earlier session that a sync
/// ends (review P1-5) are saved after the stops are installed, outside the
/// monitor's lock. The save ran under it, so a writer holding the database
/// held the runtime and every leg for as long. Now the stops are installed
/// at once, a breach is judged meanwhile, and the ended orders are saved
/// once the database is free.
#[test]
fn an_earlier_sessions_exit_is_saved_ended_without_holding_the_monitor() {
    crate::on_watched_runtime(|runtime| async move {
        let m = monitor();
        m.store.upsert_sl(&leg("live")).unwrap();
        m.mon.sync().await;
        // Its high and low noted and saved at 100, then its stop is hit.
        m.mon.process_tick(OPT, "NFO", 100.0).await;
        m.mon.wait_idle().await;
        m.mon.process_tick(OPT, "NFO", 94.0).await;
        m.mon.wait_idle().await;
        assert_eq!(m.gw.placed.lock().len(), 1);
        // The app is closed overnight; the order expired with the session.
        // A short sandbox leg was set meanwhile, its stop at 99.
        m.mon.stop().await;
        m.clock.advance(chrono::Duration::days(1));
        let mut sandbox = leg("analyze");
        sandbox.side = Some("SELL".into());
        sandbox.initial_sl = Some(99.0);
        m.store.upsert_sl(&sandbox).unwrap();
        let saved = || {
            m.store.active_sl(Some("live")).unwrap()[0]
                .exit_attempt
                .clone()
                .unwrap_or_default()
        };
        assert!(saved().contains("\"working\""), "{}", saved());
        let writer = hold_the_writes(&m.db, 800);
        let (seen, hold) = runtime
            .longest_hold(async {
                let mon = m.mon.clone();
                let sync = tokio::spawn(async move { mon.sync().await });
                let installed = within(Duration::from_millis(500), || m.mon.leg_count() == 2).await;
                // The sandbox leg's stop; the live leg's is not hit, nor
                // its high or low moved (nothing of it is saved).
                m.mon.process_tick(OPT, "NFO", 100.0).await;
                let judged = within(Duration::from_millis(500), || {
                    m.gw.books.lock().contains(&RunMode::Sandbox)
                })
                .await;
                let unsaved = saved().contains("\"working\"");
                assert!(sync.await.unwrap(), "the stops were read");
                (installed, judged, unsaved)
            })
            .await;
        join(writer).await;
        assert!(
            hold < Duration::from_millis(200),
            "the runtime was held for {:?}",
            hold
        );
        assert_eq!(
            seen,
            (true, true, true),
            "(installed, judged, still unsaved)"
        );
        let a = m.mon.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
        assert!(a.children[0].terminal(), "{:?}", a);
        assert!(!saved().contains("\"working\""), "saved ended: {}", saved());
        m.mon.wait_idle().await;
    })
}

/// PROD-REL-02 follow-up: a read of the stops is installed only if no leg
/// was cleared while it ran. Here the leg's exit fills and the book shows it
/// flat after the sync has read its row and before it installs what it
/// read: the stops are read again, and the closed leg is not brought back
/// (nor its price subscription), so nothing more is sent for it.
#[tokio::test]
async fn a_leg_cleared_while_the_stops_are_read_is_not_brought_back() {
    let m = monitor();
    m.store.upsert_sl(&leg("live")).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 95.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
    *m.gw.net.lock() = 0;
    let reads = m.mon.stop_reads();
    // Every connection held while the sync begins its read, so the read
    // waits on the blocking pool and the sync's first poll ends there: a read
    // that finished before the sync awaited it would let that same poll run
    // on to install (it did on CI's Linux runner).
    let holder = hold_the_pool(&m.db, 200);
    let mon = m.mon.clone();
    let sync = tokio::spawn(async move { mon.sync().await });
    // The sync begins its read; it cannot install what it read until this
    // test yields, and the exit is folded and the leg cleared on another
    // thread meanwhile (the fold saves and the clear deletes on the
    // blocking pool, which would otherwise yield to the sync).
    tokio::task::yield_now().await;
    holder.join().unwrap();
    wait_here(|| m.mon.stop_reads() > reads);
    assert!(!sync.is_finished());
    let (mon, filled) = (
        m.mon.clone(),
        book(vec![order_row("X1", "SELL", 65, "complete")], m.clock.now()),
    );
    elsewhere(async move { mon.reconcile_book(RunMode::Live, &filled).await });
    assert_eq!(m.mon.leg_count(), 0, "cleared");
    assert!(m.store.active_sl(None).unwrap().is_empty());
    sync.await.unwrap();
    assert_eq!(m.mon.leg_count(), 0, "the closed leg is not brought back");
    assert!(m.mon.subscribed().is_empty());
    assert_eq!(m.mon.stop_reads(), reads + 2, "read again");
    m.clock.advance(chrono::Duration::seconds(10));
    m.mon.process_tick(OPT, "NFO", 90.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.gw.placed.lock().len(), 1);
}

/// A sync never puts back an older trail over the one this monitor moved:
/// the row gets a move only every PERSIST_THROTTLE_MS. A sync between two
/// moves set the stop back to the last one written, and the pull-back
/// through the newer stop sent no exit.
#[tokio::test]
async fn a_sync_keeps_the_trail_the_monitor_moved() {
    let m = monitor();
    let mut l = leg("analyze");
    l.trailing_enabled = Some(true);
    l.trailing_step = Some(5.0);
    m.store.upsert_sl(&l).unwrap();
    m.mon.sync().await;
    m.mon.process_tick(OPT, "NFO", 110.0).await;
    m.mon.wait_idle().await;
    m.mon.process_tick(OPT, "NFO", 112.0).await;
    m.mon.wait_idle().await;
    assert_eq!(m.store.active_sl(None).unwrap()[0].current_sl, Some(105.0));
    m.mon.sync().await;
    let s = m.mon.leg("analyze", OPT, "NFO", "MIS").unwrap();
    assert_eq!((s.current_sl, s.highest_price), (Some(107.0), Some(112.0)));
    m.mon.process_tick(OPT, "NFO", 107.0).await;
    m.mon.wait_idle().await;
    let placed = m.gw.placed.lock().clone();
    assert_eq!(placed.len(), 1, "{:?}", placed);
    assert_eq!(
        (
            placed[0].0,
            placed[0].1.action.as_str(),
            placed[0].1.quantity
        ),
        (RunMode::Sandbox, "SELL", 65)
    );
}

/// PROD-REL-02 follow-up: the order reconciler asks its owners whether they
/// hold open orders (the scalping page, the runner and the strategies answer
/// from their saved rows) on the blocking pool. It asked on its async worker
/// every pass, so with every pooled connection taken the runtime waited
/// with it. Afterwards the unconfirmed entry is settled from the book once,
/// and the page trades again.
#[test]
fn a_reconcile_pass_waiting_for_the_database_never_holds_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        let h = H::new().await;
        h.ctx.reconciler.stop();
        let (cookie, csrf) = session(&h);
        h.mock.after_send.lock().push_back((
            openalgo_desktop_lib::brokers::mock::AfterSend::Dropped,
            "complete",
        ));
        *h.mock.after_send_time.lock() = "2026-10-03 09:42:05".into();
        let order = || {
            req(
                Method::POST,
                "/scalping/api/order",
                Some(page_entry()),
                Some(&cookie),
                Some(&csrf),
            )
        };
        let (s, b) = call(&h, order()).await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{}", b);
        runtime.until_idle().await;
        let holder = hold_the_pool(&h.ctx.sqlite, 600);
        let (_, hold) = runtime.longest_hold(h.ctx.reconciler.pass()).await;
        holder.join().unwrap();
        assert!(
            hold < Duration::from_millis(200),
            "the runtime was held for {:?}",
            hold
        );
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
        assert_eq!(placed(&h).len(), 1, "settled from the book, not sent again");
        let (s, b) = call(&h, order()).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(placed(&h).len(), 2);
        h.shutdown().await;
    })
}

/// Where `hold_the_pool_at_the_book_read` takes the pool, and its holder.
static POOL_AT_BOOK_READ: Mutex<Option<Arc<SqliteDb>>> = Mutex::new(None);
static POOL_HOLDER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// The mock broker's hook: the first order-book read takes every pooled
/// connection for 800 ms, so whatever the pass does next with the database
/// waits for it.
fn hold_the_pool_at_the_book_read(call: &MockCall) {
    if !matches!(call, MockCall::OrderBook) {
        return;
    }
    if let Some(db) = POOL_AT_BOOK_READ.lock().take() {
        *POOL_HOLDER.lock() = Some(hold_the_pool(&db, 800));
    }
}

/// PROD-REL-02 follow-up: which of the book's orders already belong to an
/// owner is read once per pass on the blocking pool, and matching reads it
/// from memory. Matching asked every owner for each candidate on the pass's
/// async worker (the strategies, the runner and the scalping page answer
/// from their saved rows), so with every pooled connection taken right
/// after the book was read the runtime waited with the pool. Afterwards the
/// unconfirmed exit is found in the book, once, and nothing is sent again.
#[test]
fn a_pass_matches_the_book_without_reading_owners_on_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        use openalgo_desktop_lib::brokers::mock::AfterSend;
        use openalgo_desktop_lib::scalping::monitor::ChildState;
        let h = H::new().await;
        h.ctx.reconciler.stop();
        *h.mock.positions.lock() = Some(Ok(vec![pos(OPT, "NFO", "MIS", 65)]));
        let (cookie, csrf) = session(&h);
        let body = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "side": "BUY",
            "quantity": 65, "entry_price": 100, "initial_sl": 95});
        let (s, v) = call(
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
        assert_eq!(s, StatusCode::OK, "{}", v);
        let monitor = h.ctx.scalping.monitor.clone();
        monitor.sync().await;
        // The exit reaches the broker and its answer is lost.
        h.mock
            .after_send
            .lock()
            .push_back((AfterSend::Dropped, "complete"));
        *h.mock.after_send_time.lock() = "2026-10-03 09:42:05".into();
        monitor.process_tick(OPT, "NFO", 90.0).await;
        monitor.wait_idle().await;
        let a = monitor.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
        assert_eq!(a.children[0].state, ChildState::Unconfirmed, "{:?}", a);
        // Every pooled connection opened now, so taking them later is quick.
        hold_the_pool(&h.ctx.sqlite, 0).join().unwrap();
        *POOL_AT_BOOK_READ.lock() = Some(h.ctx.sqlite.clone());
        *h.mock.on_call.lock() = Some(hold_the_pool_at_the_book_read);
        runtime.until_idle().await;
        let (_, hold) = runtime.longest_hold(h.ctx.reconciler.pass()).await;
        *h.mock.on_call.lock() = None;
        let holder = POOL_HOLDER.lock().take();
        join(holder.expect("the pass read the order book")).await;
        assert!(
            hold < Duration::from_millis(200),
            "the runtime was held for {:?}",
            hold
        );
        let a = monitor.exit_attempt("live", OPT, "NFO", "MIS").unwrap();
        assert_eq!(a.children[0].order_id.as_deref(), Some("MOCK-1"), "{:?}", a);
        assert_eq!(a.children[0].state, ChildState::Filled, "{:?}", a);
        assert_eq!(
            placed(&h),
            vec![("SELL".to_string(), 65)],
            "never sent again"
        );
        h.shutdown().await;
    })
}

/// PROD-REL-02 follow-up: an owner whose own orders cannot be read settled
/// nothing from the book, so a gap in order facts stays open (new entries
/// keep waiting) until a pass can read them. The scalping page and the
/// runner answered such a failure as nothing unresolved and the gap closed;
/// the strategies answered "holds nothing", so a lost fact opened no gap at
/// all. Each owner's table is made unreadable in turn.
#[tokio::test]
async fn a_gap_stays_open_while_an_owners_orders_cannot_be_read() {
    use openalgo_desktop_lib::services::order_reconciler::{EntryGate, GapCause};
    use std::sync::atomic::Ordering::SeqCst;
    let h = H::new().await;
    let r = h.ctx.reconciler.clone();
    r.stop();
    let rename = |from: &str, to: &str| {
        h.ctx
            .sqlite
            .conn()
            .unwrap()
            .execute_batch(&format!("ALTER TABLE {} RENAME TO {}", from, to))
            .unwrap()
    };
    // Per table: (gap opened, book read, gap kept after it, closed once
    // readable again).
    let mut seen = Vec::new();
    for table in [
        "scalping_unconfirmed",
        "openscript_orders",
        "sm_strategy_run",
    ] {
        let hidden = format!("{}_unreadable", table);
        rename(table, &hidden);
        r.open_gap(RunMode::Live, GapCause::EventLoss).await;
        let opened = r.gap_open(RunMode::Live);
        let (reads, failures) = (
            r.stats.book_reads.load(SeqCst),
            r.stats.book_read_failures.load(SeqCst),
        );
        r.pass().await;
        let read = r.stats.book_reads.load(SeqCst) > reads
            && r.stats.book_read_failures.load(SeqCst) == failures;
        let kept = r.gap_open(RunMode::Live) && r.entry_refusal(RunMode::Live).is_some();
        rename(&hidden, table);
        r.pass().await;
        seen.push((table, opened, read, kept, !r.gap_open(RunMode::Live)));
    }
    assert_eq!(
        seen,
        [
            ("scalping_unconfirmed", true, true, true, true),
            ("openscript_orders", true, true, true, true),
            ("sm_strategy_run", true, true, true, true),
        ]
    );
    h.shutdown().await;
}

/// LOG-07 defence in depth: a Live stop set while Zerodha (AB1234) was
/// connected is not exited through another account's session. Nothing is
/// read or sent there, the stop stays and the trader is told; back on its
/// own session the exit goes out. Before, the breach read the other
/// account's position book and sent the exit through it.
#[tokio::test]
async fn a_live_stop_is_not_exited_through_another_accounts_session() {
    let h = H::new().await;
    *h.mock.positions.lock() = Some(Ok(vec![pos(OPT, "NFO", "MIS", 65)]));
    let (cookie, csrf) = session(&h);
    let body = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "side": "BUY",
        "quantity": 65, "entry_price": 100, "initial_sl": 95});
    let (s, v) = call(
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
    assert_eq!(s, StatusCode::OK, "{}", v);
    h.ctx.scalping.monitor.sync().await;
    let own = h.ctx.get_broker_session().unwrap();
    h.ctx
        .set_broker_session(Some(openalgo_desktop_lib::state::BrokerSession {
            user_id: "XY9876".into(),
            ..own.clone()
        }));
    let book_reads = || {
        h.mock
            .calls()
            .iter()
            .filter(|c| matches!(c, MockCall::Positions))
            .count()
    };
    let reads = book_reads();
    h.ctx.scalping.monitor.process_tick(OPT, "NFO", 90.0).await;
    h.ctx.scalping.monitor.wait_idle().await;
    assert!(placed(&h).is_empty(), "nothing sent through XY9876");
    assert_eq!(book_reads(), reads, "its position book was not read");
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 1, "still protected");
    assert_eq!(
        h.ctx.scalping.store.active_sl(Some("live")).unwrap().len(),
        1
    );

    // Its own session again: after the retry cooldown the exit goes out.
    h.ctx.set_broker_session(Some(own));
    h.clock.advance(chrono::Duration::seconds(4));
    h.ctx.scalping.monitor.process_tick(OPT, "NFO", 90.0).await;
    h.ctx.scalping.monitor.wait_idle().await;
    assert_eq!(placed(&h), vec![("SELL".to_string(), 65)]);
    h.shutdown().await;
}

/// Lead's third review of #165, P2-2: a Live stop recorded before accounts
/// were derived from the saved keys carries the broker's placeholder as its
/// account. The same broker's session, now told by its saved keys, still
/// exits it. Before, the two values refused the exit and the stop stayed
/// with nothing able to act on it.
#[tokio::test]
async fn a_stop_recorded_with_a_placeholder_account_is_exited_through_its_brokers_keys() {
    use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
    let h = H::new().await;
    *h.mock.positions.lock() = Some(Ok(vec![pos(OPT, "NFO", "MIS", 65)]));
    let (cookie, csrf) = session(&h);
    let body = json!({"symbol": OPT, "exchange": "NFO", "product": "MIS", "side": "BUY",
        "quantity": 65, "entry_price": 100, "initial_sl": 95});
    let (s, v) = call(
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
    assert_eq!(s, StatusCode::OK, "{}", v);
    let conn = h.ctx.sqlite.conn().unwrap();
    conn.execute_batch("UPDATE scalping_sl_state SET broker_account = 'zerodha'")
        .unwrap();
    credentials::save(
        &conn,
        &h.ctx.security,
        "zerodha",
        CredentialUpdate {
            api_key: Some("kitekey".into()),
            api_secret: Some("kitesecret".into()),
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    h.ctx.scalping.monitor.sync().await;
    let own = h.ctx.get_broker_session().unwrap();
    h.ctx
        .set_broker_session(Some(openalgo_desktop_lib::state::BrokerSession {
            user_id: String::new(),
            ..own
        }));
    h.ctx.scalping.monitor.process_tick(OPT, "NFO", 90.0).await;
    h.ctx.scalping.monitor.wait_idle().await;
    assert_eq!(placed(&h), vec![("SELL".to_string(), 65)]);
    h.shutdown().await;
}

/// Re-audit NEW-TR-07: the saved stops cannot be read. The list answers an
/// error the page shows as out of date, never a successful empty list (which
/// reads as "nothing is protected"), and the running monitor keeps the stop
/// it holds. Readable again, the list shows the stop.
#[tokio::test]
async fn an_unreadable_stop_list_is_unavailable_not_empty() {
    use openalgo_desktop_lib::scalping::service::STOPS_UNAVAILABLE;
    let h = H::new().await;
    let (cookie, _) = session(&h);
    h.ctx.scalping.store.upsert_sl(&leg("live")).unwrap();
    h.ctx.scalping.monitor.sync().await;
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 1);
    let rename = |from: &str, to: &str| {
        h.ctx
            .sqlite
            .conn()
            .unwrap()
            .execute_batch(&format!("ALTER TABLE {} RENAME TO {}", from, to))
            .unwrap()
    };
    rename("scalping_sl_state", "scalping_sl_state_unreadable");
    let (s, v) = call(
        &h,
        req(Method::GET, "/scalping/api/sl", None, Some(&cookie), None),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{}", v);
    assert_eq!(v["status"], "error");
    assert_eq!(v["message"], STOPS_UNAVAILABLE);
    assert!(v.get("data").is_none(), "no list to read as empty: {}", v);
    // The monitor keeps the stop it holds, through a sync that cannot read.
    h.ctx.scalping.monitor.sync().await;
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 1, "still protected");

    rename("scalping_sl_state_unreadable", "scalping_sl_state");
    let (s, v) = call(
        &h,
        req(Method::GET, "/scalping/api/sl", None, Some(&cookie), None),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"].as_array().unwrap().len(), 1, "{}", v);
    assert_eq!(h.ctx.scalping.monitor.leg_count(), 1);
    h.shutdown().await;
}
