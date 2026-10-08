//! Web-UI routes, batch 2: search, page order actions, Action Center and
//! Semi-Auto routing, sandbox pages, analyzer log, P&L tracker, chart test,
//! watchlists, the alert log and the dashboard funds. Driven in-process
//! against the full router with the mock broker connected.
//!
//! Self-contained (no shared support module) so it can be folded into the
//! consolidated integration crate unchanged.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::{MockBroker, MockCall};
use openalgo_desktop_lib::brokers::types::{Candle, Order, Position, Quote, Trade};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::db::sqlite::action_center;
use openalgo_desktop_lib::events::subscribers::socketio::translate;
use openalgo_desktop_lib::events::{Event, Lane, Subscriber, Topic};
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::action_center_service;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::auth_service::AuthService;
use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

const USER: &str = "trader";

fn now() -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(2026, 10, 5, 10, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn sym(symbol: &str, exchange: &str, name: &str, expiry: &str, strike: f64, it: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: format!("{}:{}", exchange, symbol),
        expiry: expiry.into(),
        strike,
        lot_size: if exchange == "NFO" { 65 } else { 1 },
        instrument_type: it.into(),
        tick_size: 0.05,
    }
}

fn master() -> Vec<SymToken> {
    vec![
        sym("SBIN", "NSE", "STATE BANK OF INDIA", "", 0.0, "EQ"),
        sym("INFY", "NSE", "INFOSYS", "", 0.0, "EQ"),
        sym("SBIN", "BSE", "STATE BANK OF INDIA", "", 0.0, "EQ"),
        sym("NIFTY", "NSE_INDEX", "NIFTY 50", "", 0.0, "EQ"),
        sym("NIFTY27OCT26FUT", "NFO", "NIFTY", "27-OCT-26", 0.0, "FUT"),
        sym(
            "NIFTY27OCT2622000CE",
            "NFO",
            "NIFTY",
            "27-OCT-26",
            22000.0,
            "CE",
        ),
        sym(
            "NIFTY27OCT2622000PE",
            "NFO",
            "NIFTY",
            "27-OCT-26",
            22000.0,
            "PE",
        ),
        sym(
            "NIFTY03NOV2622000CE",
            "NFO",
            "NIFTY",
            "03-NOV-26",
            22000.0,
            "CE",
        ),
        sym(
            "NIFTY29SEP2622000CE",
            "NFO",
            "NIFTY",
            "29-SEP-26",
            22000.0,
            "CE",
        ),
        sym(
            "CRUDEOIL19NOV26FUT",
            "MCX",
            "CRUDEOIL",
            "19-NOV-26",
            0.0,
            "FUT",
        ),
        sym(
            "011NSETEST27OCT26100CE",
            "NFO",
            "011NSETEST",
            "27-OCT-26",
            100.0,
            "CE",
        ),
    ]
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Arc<Event>>>,
}

#[async_trait::async_trait]
impl Subscriber for Recorder {
    fn name(&self) -> &'static str {
        "webui2-recorder"
    }
    fn topics(&self) -> Vec<Topic> {
        vec![
            Topic::OrderPlaced,
            Topic::OrderFailed,
            Topic::OrderModified,
            Topic::OrderModifyFailed,
            Topic::OrderCancelled,
            Topic::OrderCancelFailed,
            Topic::AllOrdersCancelled,
            Topic::PositionClosed,
            Topic::GttCancelled,
            Topic::PendingOrderCreated,
            Topic::PendingOrderUpdated,
        ]
    }
    async fn handle(&self, event: Arc<Event>) {
        self.events.lock().push(event);
    }
}

impl Recorder {
    async fn wait(&self, topic: &str, n: usize) -> Vec<Arc<Event>> {
        for _ in 0..400 {
            let got: Vec<Arc<Event>> = self
                .events
                .lock()
                .iter()
                .filter(|e| e.topic().as_str() == topic)
                .cloned()
                .collect();
            if got.len() >= n {
                return got;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        self.events
            .lock()
            .iter()
            .filter(|e| e.topic().as_str() == topic)
            .cloned()
            .collect()
    }

    fn count(&self, topic: &str) -> usize {
        self.events
            .lock()
            .iter()
            .filter(|e| e.topic().as_str() == topic)
            .count()
    }
}

struct H {
    ctx: Arc<AppState>,
    mock: Arc<MockBroker>,
    key: String,
    cookie: String,
    csrf: String,
    rec: Arc<Recorder>,
    _dir: tempfile::TempDir,
}

impl H {
    /// Signed in, API key made, mock broker connected unless `broker` is false.
    async fn new(broker: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let symbols = SymbolResolver::new();
        let mock = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: ManualClock::new(now()),
                brokers: Arc::new(BrokerRegistry::with_symbols(
                    symbols,
                    vec![mock.clone() as Arc<dyn Broker>],
                )),
            },
        )
        .unwrap();
        let rows = master();
        for r in &rows {
            mock.set_quote(Quote {
                symbol: r.symbol.clone(),
                exchange: r.exchange.clone(),
                ltp: 100.0,
                close: 100.0,
                ..Default::default()
            });
        }
        ctx.load_symbol_cache(rows);
        AuthService::setup(&ctx, USER, "trader@example.com", "Secret@123").unwrap();
        let key = ApiKeyService::current(&ctx)
            .unwrap()
            .unwrap()
            .expose()
            .to_string();
        if broker {
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
        ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
        let rec = Arc::new(Recorder::default());
        ctx.bus.subscribe(rec.clone(), Lane::Critical);
        H {
            ctx,
            mock,
            key,
            cookie: format!("session={}", s.id),
            csrf: s.csrf_token,
            rec,
            _dir: dir,
        }
    }

    fn semi_auto(&self) {
        ApiKeyService::set_order_mode(&self.ctx, "semi_auto").unwrap();
    }

    fn analyze(&self, on: bool) {
        self.ctx.sqlite.set_analyze_mode(on).unwrap();
    }

    async fn raw(&self, mut req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        let resp = openalgo_desktop_lib::server::app(self.ctx.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    fn build(
        &self,
        m: Method,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
        csrf: Option<&str>,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(m)
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        }
        if let Some(t) = csrf {
            b = b.header("x-csrftoken", t);
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        b.body(body).unwrap()
    }

    /// As the signed-in trader, with the CSRF token.
    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = self.build(m, path, body, Some(&self.cookie), Some(&self.csrf));
        let (s, _, b) = self.raw(req).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(body)).await
    }

    async fn api(&self, path: &str, mut body: Value) -> (StatusCode, Value) {
        body.as_object_mut()
            .unwrap()
            .insert("apikey".into(), json!(self.key));
        let req = self.build(Method::POST, path, Some(body), None, None);
        let (s, _, b) = self.raw(req).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    fn places(&self) -> usize {
        self.mock
            .calls()
            .iter()
            .filter(|c| matches!(c, MockCall::PlaceOrder(_)))
            .count()
    }
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn set(k: &[&str]) -> BTreeSet<String> {
    k.iter().map(|s| s.to_string()).collect()
}

fn position(symbol: &str, qty: i32, avg: f64, pnl: f64) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: qty,
        overnight_quantity: 0,
        average_price: avg,
        ltp: 100.0,
        pnl,
        realized_pnl: 0.0,
        unrealized_pnl: pnl,
        buy_quantity: 0,
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

fn order_row(id: &str, status: &str) -> Order {
    Order {
        order_id: id.into(),
        exchange_order_id: None,
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        filled_quantity: 0,
        pending_quantity: 1,
        price: 99.0,
        trigger_price: 0.0,
        average_price: 0.0,
        order_type: "LIMIT".into(),
        product: "MIS".into(),
        status: status.into(),
        validity: "DAY".into(),
        order_timestamp: "2026-10-05 09:30:00".into(),
        exchange_timestamp: None,
        rejection_reason: None,
    }
}

const PLACE: &str = r#"{"strategy": "test", "symbol": "SBIN", "exchange": "NSE", "action": "BUY",
    "quantity": 1, "pricetype": "MARKET", "product": "MIS"}"#;

fn place_body() -> Value {
    serde_json::from_str(PLACE).unwrap()
}

// ------------------------------------------------------------------ access

const ROUTES: &[(&str, &str)] = &[
    ("GET", "/search/api/search?q=SBIN"),
    ("GET", "/search/api/expiries"),
    ("GET", "/search/api/underlyings"),
    ("POST", "/close_position"),
    ("POST", "/close_all_positions"),
    ("POST", "/cancel_all_orders"),
    ("POST", "/cancel_order"),
    ("POST", "/modify_order"),
    ("POST", "/modify_gtt_order"),
    ("POST", "/cancel_gtt_order"),
    ("POST", "/action-center/approve/1"),
    ("POST", "/action-center/reject/1"),
    ("DELETE", "/action-center/delete/1"),
    ("GET", "/action-center/count"),
    ("POST", "/action-center/approve-all"),
    ("GET", "/action-center/api/data"),
    ("GET", "/sandbox/api/configs"),
    ("POST", "/sandbox/update"),
    ("POST", "/sandbox/reset"),
    ("POST", "/sandbox/reload-squareoff"),
    ("GET", "/sandbox/squareoff-status"),
    ("GET", "/sandbox/mypnl/api/data"),
    ("GET", "/sandbox/mypnl/export/daily"),
    ("GET", "/sandbox/mypnl/export/positions"),
    ("GET", "/sandbox/mypnl/export/holdings"),
    ("GET", "/sandbox/mypnl/export/trades"),
    ("GET", "/analyzer/api/data"),
    ("GET", "/analyzer/export"),
    ("POST", "/pnltracker/api/pnl"),
    ("GET", "/chart/test/api/history?symbol=SBIN&exchange=NSE"),
    ("GET", "/watchlist/api/lists"),
    ("POST", "/watchlist/api/lists"),
    ("PATCH", "/watchlist/api/lists/1"),
    ("DELETE", "/watchlist/api/lists/1"),
    ("POST", "/watchlist/api/lists/1/clear"),
    ("POST", "/watchlist/api/lists/1/items"),
    ("DELETE", "/watchlist/api/lists/1/items/2"),
    ("PUT", "/watchlist/api/lists/1/items/order"),
    ("POST", "/alerts/fired"),
    ("GET", "/alerts/log"),
    ("DELETE", "/alerts/log"),
];

#[tokio::test]
async fn every_route_needs_the_user_and_writes_need_csrf() {
    let h = H::new(true).await;
    let anon = h.ctx.sessions.create(h.ctx.now());
    let anon_cookie = format!("session={}", anon.id);
    for (m, p) in ROUTES {
        let m = Method::from_bytes(m.as_bytes()).unwrap();
        let req = h.build(
            m.clone(),
            p,
            Some(json!({})),
            Some(&anon_cookie),
            Some(&anon.csrf_token),
        );
        let (s, _, b) = h.raw(req).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{} {}", m, p);
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["message"], "Not authenticated", "{} {}", m, p);
        if m != Method::GET {
            let req = h.build(m.clone(), p, Some(json!({})), Some(&h.cookie), None);
            let (s, _, b) = h.raw(req).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{} {}", m, p);
            let v: Value = serde_json::from_slice(&b).unwrap();
            assert!(
                v["message"]
                    .as_str()
                    .unwrap()
                    .contains("session has expired"),
                "{} {}",
                m,
                p
            );
        }
    }
    assert_eq!(h.places(), 0, "no refused request reached the broker");
}

// ------------------------------------------------------------------ search

#[tokio::test]
async fn search_expiries_and_underlyings_have_the_web_shapes() {
    let h = H::new(true).await;
    let (s, v) = h.get("/search/api/search?q=sbin").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total"], 2);
    assert_eq!(
        keys(&v["results"][0]),
        set(&[
            "symbol",
            "brsymbol",
            "name",
            "exchange",
            "brexchange",
            "token",
            "expiry",
            "strike",
            "lotsize",
            "contract_value",
            "instrumenttype",
            "freeze_qty"
        ])
    );
    let (_, v) = h.get("/search/api/search?q=sbin&exchange=NSE,BSE").await;
    assert_eq!(v["total"], 2);
    let (_, v) = h.get("/search/api/search").await;
    assert_eq!(v, json!({"results": [], "total": 0}));
    let (_, v) = h
        .get("/search/api/search?exchange=NFO&instrumenttype=CE&underlying=nifty")
        .await;
    let syms: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(
        syms,
        [
            "NIFTY03NOV2622000CE",
            "NIFTY27OCT2622000CE",
            "NIFTY29SEP2622000CE"
        ]
    );
    let (_, v) = h.get("/search/api/search?q=22000&exchange=NFO").await;
    assert_eq!(v["total"], 4);
    let (_, v) = h
        .get("/search/api/expiries?exchange=NFO&underlying=NIFTY")
        .await;
    assert_eq!(
        v,
        json!({"status": "success", "expiries": ["27-OCT-26", "03-NOV-26"]})
    );
    let (_, v) = h
        .get("/search/api/expiries?exchange=NFO&underlying=NIFTY&instrumenttype=options")
        .await;
    assert_eq!(
        v["expiries"],
        json!(["29-SEP-26", "27-OCT-26", "03-NOV-26"])
    );
    let (_, v) = h.get("/search/api/underlyings?exchange=NFO").await;
    assert_eq!(v, json!({"status": "success", "underlyings": ["NIFTY"]}));
    let (_, v) = h
        .get("/search/api/underlyings?exchange=MCX&include_futures=true")
        .await;
    assert_eq!(v["underlyings"], json!(["CRUDEOIL"]));
    let (_, v) = h.get("/search/api/underlyings?exchange=MCX").await;
    assert_eq!(v["underlyings"], json!([]));
}

// ------------------------------------------------------------------ page order actions

#[tokio::test]
async fn page_order_actions_validate_and_need_the_broker() {
    let h = H::new(false).await;
    for p in [
        "/close_all_positions",
        "/cancel_all_orders",
        "/cancel_order",
        "/modify_order",
        "/modify_gtt_order",
        "/cancel_gtt_order",
    ] {
        let (s, v) = h.post(p, json!({"orderid": "1", "trigger_id": "1"})).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", p);
        assert_eq!(
            v,
            json!({"status": "error", "message": "Authentication error"})
        );
    }
    let h = H::new(true).await;
    let (s, v) = h.post("/close_position", json!({"symbol": "SBIN"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v["message"],
        "Missing required parameters (symbol, exchange, product)"
    );
    let (s, v) = h.post("/cancel_order", json!({})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Order ID is required"))
    );
    let (s, v) = h.post("/modify_order", json!({"symbol": "SBIN"})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Order ID is required"))
    );
    let (s, v) = h.post("/modify_gtt_order", json!({})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("trigger_id is required"))
    );
    let (s, v) = h.post("/cancel_gtt_order", json!({"trigger_id": ""})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("trigger_id is required"))
    );
}

#[tokio::test]
async fn page_order_actions_run_live_with_the_api_results_and_events() {
    let h = H::new(true).await;
    // Semi-Auto blocks these for API clients, never for the trader's own page.
    h.semi_auto();
    *h.mock.positions.lock() = Some(Ok(vec![position("SBIN", 5, 100.0, 10.0)]));
    let (s, v) = h
        .post(
            "/close_position",
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(keys(&v), set(&["status", "message", "orderid"]));
    assert_eq!(v["message"], "Position close order placed successfully.");
    let placed = h.mock.calls().into_iter().find_map(|c| match c {
        MockCall::PlaceOrder(o) => Some(o),
        _ => None,
    });
    let placed = placed.unwrap();
    assert_eq!(placed.action.as_str(), "SELL");
    assert_eq!(placed.quantity, 5);
    let ev = h.rec.wait("position.closed", 1).await;
    let (name, payload) = translate(&ev[0]).unwrap();
    assert_eq!(name, "close_position_event");
    assert_eq!(
        payload["message"],
        "Position close order placed successfully."
    );

    // Flat: nothing to exit.
    *h.mock.positions.lock() = Some(Ok(vec![]));
    let (s, v) = h
        .post(
            "/close_position",
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v["message"],
        "No OpenPosition Found. Not placing Exit order."
    );

    let (s, v) = h.post("/cancel_order", json!({"orderid": "42"})).await;
    assert_eq!(
        (s, &v),
        (
            StatusCode::OK,
            &json!({"status": "success", "orderid": "42"})
        )
    );
    h.rec.wait("order.cancelled", 1).await;

    let (s, v) = h
        .post("/modify_order", json!({"orderid": "42", "symbol": "SBIN", "exchange": "NSE",
            "action": "BUY", "product": "MIS", "pricetype": "LIMIT", "price": "101.5", "quantity": "2"}))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["orderid"], "42");
    let modified = h.mock.calls().into_iter().find_map(|c| match c {
        MockCall::ModifyOrder(m) => Some(m),
        _ => None,
    });
    let m = format!("{:?}", modified.unwrap());
    assert!(m.contains("101.5") && m.contains("quantity: 2"), "{}", m);
    h.rec.wait("order.modified", 1).await;

    *h.mock.order_book.lock() = Some(Ok(vec![order_row("1", "open"), order_row("2", "complete")]));
    let (s, v) = h.post("/cancel_all_orders", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], "Successfully canceled 1 orders");
    assert_eq!(v["canceled_orders"], json!(["1"]));
    *h.mock.order_book.lock() = Some(Ok(vec![]));
    let (_, v) = h.post("/cancel_all_orders", json!({})).await;
    assert_eq!(
        v,
        json!({"status": "success", "message": "Successfully canceled 0 orders",
        "canceled_orders": [], "failed_cancellations": []})
    );

    *h.mock.positions.lock() = Some(Ok(vec![position("INFY", -3, 100.0, 0.0)]));
    let (s, v) = h.post("/close_all_positions", json!({})).await;
    assert_eq!(
        (s, &v),
        (
            StatusCode::OK,
            &json!({"status": "success", "message": "All Open Positions Squared Off"})
        )
    );

    let (s, v) = h.post("/cancel_gtt_order", json!({"trigger_id": 77})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["trigger_id"], "77");
    let (s, _) = h
        .post(
            "/modify_gtt_order",
            json!({"trigger_id": "77", "symbol": "SBIN", "exchange": "NSE",
            "trigger_type": "SINGLE", "action": "SELL", "product": "CNC", "quantity": "1",
            "price": "120", "triggerprice_sl": 0, "triggerprice_tg": "121"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // No page action was queued.
    assert_eq!(h.rec.count("action_center.pending_order_created"), 0);
}

#[tokio::test]
async fn page_order_actions_go_to_the_sandbox_in_analyzer_mode() {
    let h = H::new(true).await;
    h.analyze(true);
    let (s, v) = h.post("/close_all_positions", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    let (s, v) = h.post("/cancel_order", json!({"orderid": "nope"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{}", v);
    assert_eq!(v["mode"], "analyze");
    let (s, v) = h
        .post(
            "/close_position",
            json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["mode"], "analyze");
    assert_eq!(h.places(), 0, "nothing reached the live broker");
}

// ------------------------------------------------------------------ Semi-Auto and the Action Center

#[tokio::test]
async fn semi_auto_queues_then_approve_executes_once() {
    let h = H::new(true).await;
    h.semi_auto();
    let (s, v) = h.api("/api/v1/placeorder", place_body()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Order queued for approval in Action Center",
            "mode": "semi_auto", "pending_order_id": 1})
    );
    assert_eq!(h.places(), 0, "queued, not executed");
    let ev = h.rec.wait("action_center.pending_order_created", 1).await;
    let (name, payload) = translate(&ev[0]).unwrap();
    assert_eq!(name, "pending_order_created");
    assert_eq!(
        payload,
        json!({"pending_order_id": 1, "user_id": USER, "api_type": "placeorder",
            "message": "New placeorder order queued for approval"})
    );

    let (s, v) = h.get("/action-center/api/data").await;
    assert_eq!(s, StatusCode::OK);
    let o = &v["data"]["orders"][0];
    assert_eq!(o["symbol"], "SBIN");
    assert_eq!(o["status"], "pending");
    assert!(o["raw_order_data"].get("apikey").is_none());
    assert_eq!(
        keys(o),
        set(&[
            "id",
            "user_id",
            "api_type",
            "status",
            "created_at_ist",
            "approved_at_ist",
            "approved_age_seconds",
            "approved_by",
            "rejected_at_ist",
            "rejected_by",
            "rejected_reason",
            "broker_order_id",
            "broker_status",
            "strategy",
            "raw_order_data",
            "symbol",
            "exchange",
            "action",
            "quantity",
            "price",
            "trigger_price",
            "price_type",
            "product_type"
        ])
    );
    assert_eq!(v["data"]["statistics"]["total_pending"], 1);
    assert_eq!(v["data"]["statistics"]["total_placeorder"], 1);
    let (_, c) = h.get("/action-center/count").await;
    assert_eq!(c, json!({"count": 1}));

    let (s, v) = h.post("/action-center/approve/1", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    assert_eq!(v["message"], "Order approved and executed successfully");
    assert_eq!(v["broker_order_id"], "MOCK-1");
    assert_eq!(h.places(), 1);
    let ev = h.rec.wait("action_center.pending_order_updated", 1).await;
    let (name, payload) = translate(&ev[0]).unwrap();
    assert_eq!(name, "pending_order_updated");
    assert_eq!(
        payload,
        json!({"action": "approved", "order_id": 1, "user_id": USER})
    );
    h.rec.wait("order.placed", 1).await;

    // A second approval is refused and sends nothing.
    let (s, v) = h.post("/action-center/approve/1", json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["message"], action_center_service::ALREADY_HANDLED_MESSAGE);
    assert_eq!(h.places(), 1);
    let row = action_center::get(&h.ctx.sqlite.conn().unwrap(), 1)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "approved");
    assert_eq!(row.broker_order_id.as_deref(), Some("MOCK-1"));
    assert_eq!(row.broker_status.as_deref(), Some("open"));
    let (_, v) = h.get("/action-center/api/data?status=approved").await;
    assert_eq!(v["data"]["orders"][0]["broker_status"], "open");
    assert!(v["data"]["orders"][0]["approved_age_seconds"].is_number());
    // Not pending any more, so it can be deleted.
    let (s, _) = h
        .call(Method::DELETE, "/action-center/delete/1", None)
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn racing_approvals_dispatch_once() {
    let h = H::new(true).await;
    h.semi_auto();
    h.api("/api/v1/placeorder", place_body()).await;
    let (a, b) = tokio::join!(
        h.post("/action-center/approve/1", json!({})),
        h.post("/action-center/approve/1", json!({}))
    );
    let mut statuses = vec![a.0, b.0];
    statuses.sort();
    assert_eq!(statuses, vec![StatusCode::OK, StatusCode::CONFLICT]);
    assert_eq!(h.places(), 1);

    // The execution claim on its own: two executors of one approved row.
    h.api("/api/v1/placeorder", place_body()).await;
    {
        let c = h.ctx.sqlite.conn().unwrap();
        assert!(action_center::approve(&c, 2, USER, USER, h.ctx.now()).unwrap());
    }
    let (x, y) = tokio::join!(
        action_center_service::execute_approved(&h.ctx, 2),
        action_center_service::execute_approved(&h.ctx, 2)
    );
    let wins = [x.success, y.success].iter().filter(|w| **w).count();
    assert_eq!(wins, 1);
    let loser = if x.success { &y } else { &x };
    assert_eq!(
        loser.status,
        action_center_service::ALREADY_SUBMITTING_STATUS
    );
    assert_eq!(h.places(), 2);
}

#[tokio::test]
async fn rejected_orders_never_execute() {
    let h = H::new(true).await;
    h.semi_auto();
    h.api("/api/v1/placeorder", place_body()).await;
    let (s, v) = h
        .call(Method::DELETE, "/action-center/delete/1", None)
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Failed to delete order"))
    );
    let (s, v) = h
        .post("/action-center/reject/1", json!({"reason": "not today"}))
        .await;
    assert_eq!(
        (s, &v),
        (
            StatusCode::OK,
            &json!({"status": "success", "message": "Order rejected successfully"})
        )
    );
    let ev = h.rec.wait("action_center.pending_order_updated", 1).await;
    assert_eq!(translate(&ev[0]).unwrap().1["action"], "rejected");
    let (s, _) = h.post("/action-center/approve/1", json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = h.post("/action-center/reject/1", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(h.places(), 0);
    let (_, v) = h.get("/action-center/api/data?status=rejected").await;
    assert_eq!(v["data"]["orders"][0]["rejected_reason"], "not today");
    assert_eq!(v["data"]["statistics"]["total_rejected"], 1);
    let (s, _) = h.post("/action-center/approve/abc", json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.post("/action-center/approve/99", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .call(Method::DELETE, "/action-center/delete/1", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let ev = h.rec.wait("action_center.pending_order_updated", 2).await;
    assert_eq!(translate(&ev[1]).unwrap().1["action"], "deleted");
}

#[tokio::test]
async fn approve_all_and_other_queueable_types() {
    let h = H::new(true).await;
    let (s, v) = h.post("/action-center/approve-all", json!({})).await;
    assert_eq!(
        (s, &v),
        (
            StatusCode::OK,
            &json!({"status": "info", "message": "No pending orders to approve"})
        )
    );
    h.semi_auto();
    h.api("/api/v1/placeorder", place_body()).await;
    let (s, v) = h
        .api("/api/v1/placesmartorder", json!({"strategy": "t", "symbol": "SBIN", "exchange": "NSE",
            "action": "BUY", "quantity": 1, "position_size": 1, "pricetype": "MARKET", "product": "MIS"}))
        .await;
    assert_eq!((s, v["mode"].as_str()), (StatusCode::OK, Some("semi_auto")));
    // Modify and cancel are refused in Semi-Auto for API clients, not queued.
    let (s, _) = h
        .api(
            "/api/v1/cancelorder",
            json!({"strategy": "t", "orderid": "1"}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        h.rec
            .wait("action_center.pending_order_created", 2)
            .await
            .len(),
        2
    );
    let (s, v) = h.post("/action-center/approve-all", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        set(&[
            "status",
            "message",
            "approved_count",
            "executed_count",
            "failed_executions",
            "already_handled"
        ])
    );
    assert_eq!(v["approved_count"], 2);
    assert_eq!(v["executed_count"], 2);
    assert_eq!(
        v["message"],
        "Successfully approved and executed all 2 orders"
    );
    assert_eq!(h.places(), 2);
    let ev = h.rec.wait("action_center.pending_order_updated", 1).await;
    assert_eq!(
        translate(ev.last().unwrap()).unwrap().1,
        json!({"action": "batch_approved", "user_id": USER, "count": 2})
    );
    let rows = action_center::list(&h.ctx.sqlite.conn().unwrap(), USER, Some("approved")).unwrap();
    assert!(rows
        .iter()
        .all(|r| r.broker_status.as_deref() != Some("submitting")));
}

#[tokio::test]
async fn auto_mode_executes_immediately() {
    let h = H::new(true).await;
    let (s, v) = h.api("/api/v1/placeorder", place_body()).await;
    assert_eq!(
        (s, &v),
        (
            StatusCode::OK,
            &json!({"status": "success", "orderid": "MOCK-1"})
        )
    );
    assert_eq!(h.places(), 1);
    let (_, c) = h.get("/action-center/count").await;
    assert_eq!(c["count"], 0);
}

// ------------------------------------------------------------------ sandbox pages

#[tokio::test]
async fn sandbox_settings_reset_and_pnl() {
    let h = H::new(true).await;
    let (s, v) = h.get("/sandbox/api/configs").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    assert_eq!(
        keys(&v["configs"]),
        set(&["capital", "leverage", "square_off", "intervals", "expiry"])
    );
    let (s, v) = h
        .post("/sandbox/update", json!({"config_key": "starting_capital"}))
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("Missing config_key or config_value")
        )
    );
    let (s, v) = h
        .post(
            "/sandbox/update",
            json!({"config_key": "futures_leverage", "config_value": 99}),
        )
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Leverage cannot exceed 50x"))
    );
    let (s, v) = h
        .post(
            "/sandbox/update",
            json!({"config_key": "futures_leverage", "config_value": "12"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Configuration futures_leverage updated successfully"})
    );
    let (s, v) = h.get("/sandbox/squareoff-status").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(keys(&v), set(&["status", "data", "mode"]));
    let (s, v) = h.post("/sandbox/reload-squareoff", json!({})).await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("success")));
    for kind in ["daily", "positions", "holdings", "trades"] {
        let (s, v) = h.get(&format!("/sandbox/mypnl/export/{kind}")).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{}", kind);
        assert_eq!(v["status"], "error");
    }
    let (s, _) = h.get("/sandbox/mypnl/export/other").await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // One sandbox trade, then the P&L page and the exports have rows.
    h.analyze(true);
    let (s, v) = h.api("/api/v1/placeorder", place_body()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (s, v) = h.get("/sandbox/mypnl/api/data").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        keys(&v["data"]),
        set(&["summary", "daily_pnl", "positions", "holdings", "trades"])
    );
    assert_eq!(v["data"]["trades"].as_array().unwrap().len(), 1);
    let req = h.build(
        Method::GET,
        "/sandbox/mypnl/export/trades",
        None,
        Some(&h.cookie),
        None,
    );
    let (s, hd, b) = h.raw(req).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hd[header::CONTENT_TYPE], "text/csv");
    assert!(hd[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains("sandbox_trades_"));
    let csv = String::from_utf8(b).unwrap();
    assert!(csv.starts_with(
        "Trade ID,Order ID,Symbol,Exchange,Action,Quantity,Price,Product,Strategy,Timestamp\r\n"
    ));
    assert!(csv.contains(",SBIN,NSE,BUY,1,100.0,MIS,test,"), "{}", csv);
    let (s, _) = h.get("/sandbox/mypnl/export/positions").await;
    assert_eq!(s, StatusCode::OK);

    let (s, v) = h.post("/sandbox/reset", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .starts_with("Configuration and data reset to defaults"));
    let (_, v) = h.get("/sandbox/mypnl/api/data").await;
    assert_eq!(v["data"]["trades"], json!([]));
}

// ------------------------------------------------------------------ analyzer log

#[tokio::test]
async fn analyzer_data_and_export() {
    let h = H::new(true).await;
    h.ctx
        .logs
        .insert_analyzer_log(
            "placeorder",
            &json!({"apikey": "secret", "symbol": "SBIN", "exchange": "NSE", "strategy": "s1",
                "action": "BUY", "quantity": 1, "pricetype": "MARKET", "product": "MIS"}),
            &json!({"status": "error", "message": "Invalid symbol", "mode": "analyze"}),
        )
        .unwrap();
    // Logs are stamped with the wall clock; ask for the real date.
    let today = Utc::now()
        .with_timezone(&Kolkata)
        .format("%Y-%m-%d")
        .to_string();
    let q = format!("start_date={}&end_date={}", today, today);
    let (s, v) = h.get(&format!("/analyzer/api/data?{}", q)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(keys(&v["data"]), set(&["stats", "requests"]));
    assert_eq!(
        keys(&v["data"]["stats"]),
        set(&["total_requests", "issues", "symbols", "sources"])
    );
    let r = &v["data"]["requests"][0];
    assert_eq!(r["symbol"], "SBIN");
    assert_eq!(r["source"], "s1");
    assert_eq!(r["analysis"]["issues"], true);
    assert!(r["request_data"].get("apikey").is_none());
    let (s, _) = h.get("/analyzer/api/data?start_date=05-10-2026").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let req = h.build(
        Method::GET,
        &format!("/analyzer/export?{}", q),
        None,
        Some(&h.cookie),
        None,
    );
    let (s, hd, b) = h.raw(req).await;
    assert_eq!(s, StatusCode::OK);
    assert!(hd[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains("analyzer_logs_"));
    let csv = String::from_utf8(b).unwrap();
    assert!(
        csv.contains("placeorder,s1,SBIN,NSE,BUY,1,MARKET,MIS,Error,Invalid symbol\r\n"),
        "{}",
        csv
    );
    assert!(!csv.contains("secret"));
}

// ------------------------------------------------------------------ P&L tracker and chart test

fn candles(day: (i32, u32, u32), from: (u32, u32), n: u32, start: f64) -> Vec<Candle> {
    let t0 = Kolkata
        .with_ymd_and_hms(day.0, day.1, day.2, from.0, from.1, 0)
        .unwrap()
        .timestamp();
    (0..n)
        .map(|i| Candle {
            timestamp: t0 + i64::from(i) * 60,
            open: start,
            high: start + 1.0,
            low: start - 1.0,
            close: start + f64::from(i),
            volume: 10,
            oi: 0,
        })
        .collect()
}

#[tokio::test]
async fn pnl_tracker_shapes() {
    let h = H::new(false).await;
    let (s, v) = h.post("/pnltracker/api/pnl", json!({})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("Authentication required"))
    );
    let h = H::new(true).await;
    let (s, v) = h.post("/pnltracker/api/pnl", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["current_mtm"], 0);
    assert_eq!(
        keys(&v["data"]),
        set(&[
            "current_mtm",
            "max_mtm",
            "max_mtm_time",
            "min_mtm",
            "min_mtm_time",
            "max_drawdown",
            "pnl_series",
            "drawdown_series"
        ])
    );
    *h.mock.trade_book.lock() = Some(Ok(vec![Trade {
        order_id: "1".into(),
        trade_id: "T1".into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        side: "BUY".into(),
        quantity: 10,
        average_price: 100.0,
        trade_value: 1000.0,
        timestamp: "2026-10-05 09:16:00".into(),
    }]));
    *h.mock.history.lock() = Some(Ok(candles((2026, 10, 5), (9, 16), 5, 100.0)));
    let (s, v) = h.post("/pnltracker/api/pnl", json!({})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let d = &v["data"];
    assert_eq!(d["current_mtm"], 40.0);
    assert_eq!(d["max_mtm_time"], "09:20");
    assert_eq!(d["pnl_series"][0]["value"], 0.0, "09:15 is zero-filled");
    assert_eq!(keys(&d["pnl_series"][0]), set(&["time", "value"]));
}

#[tokio::test]
async fn chart_test_history() {
    let h = H::new(true).await;
    let (s, v) = h.get("/chart/test/api/history?symbol=sbin").await;
    assert_eq!(
        (s, v["message"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("symbol and exchange are required")
        )
    );
    let mut bars = candles((2026, 10, 2), (9, 15), 2, 100.0);
    bars.extend(candles((2026, 10, 5), (9, 15), 3, 200.0));
    *h.mock.history.lock() = Some(Ok(bars));
    let (s, v) = h
        .get("/chart/test/api/history?symbol=sbin&exchange=nse&interval=1m")
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        keys(&v),
        set(&["status", "symbol", "exchange", "interval", "date", "candles"])
    );
    assert_eq!(v["symbol"], "SBIN");
    assert_eq!(v["date"], "2026-10-05");
    assert_eq!(v["candles"].as_array().unwrap().len(), 3);
    assert_eq!(
        keys(&v["candles"][0]),
        set(&["time", "open", "high", "low", "close", "volume"])
    );
    let (_, v) = h
        .get("/chart/test/api/history?symbol=SBIN&exchange=NSE&interval=5m")
        .await;
    assert_eq!(v["candles"].as_array().unwrap().len(), 5);
    *h.mock.history.lock() = Some(Err("no data".into()));
    let (s, v) = h
        .get("/chart/test/api/history?symbol=SBIN&exchange=NSE")
        .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(v["status"], "error");
}

// ------------------------------------------------------------------ watchlists and alerts

#[tokio::test]
async fn watchlist_crud() {
    let h = H::new(true).await;
    let (s, v) = h.post("/watchlist/api/lists", json!({"name": " "})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("Name is required"))
    );
    let (s, v) = h
        .post("/watchlist/api/lists", json!({"name": "x".repeat(65)}))
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("Name must be 64 characters or fewer")
        )
    );
    let (s, v) = h
        .post(
            "/watchlist/api/lists",
            json!({"name": "Main", "items": "nope"}),
        )
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("items must be a list"))
    );
    let (s, v) = h
        .post(
            "/watchlist/api/lists",
            json!({"name": "Main", "items": [{"symbol": "sbin", "exchange": "nse"}]}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        v["data"]["items"][0],
        json!({"id": 1, "symbol": "SBIN", "exchange": "NSE", "position": 0})
    );
    assert_eq!(keys(&v["data"]), set(&["id", "name", "position", "items"]));
    let (s, v) = h
        .post("/watchlist/api/lists", json!({"name": "Main"}))
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (
            StatusCode::CONFLICT,
            Some("A list named \"Main\" already exists")
        )
    );
    let (s, v) = h
        .post(
            "/watchlist/api/lists/1/items",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["data"]["position"], 1);
    let (s, _) = h
        .post("/watchlist/api/lists/1/items", json!({"symbol": "INFY"}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .post(
            "/watchlist/api/lists/9/items",
            json!({"symbol": "INFY", "exchange": "NSE"}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = h
        .call(
            Method::PUT,
            "/watchlist/api/lists/1/items/order",
            Some(json!({"order": [2, 1]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .call(
            Method::PUT,
            "/watchlist/api/lists/1/items/order",
            Some(json!({"order": 2})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = h.get("/watchlist/api/lists").await;
    assert_eq!(v["data"][0]["items"][0]["symbol"], "INFY");
    let (s, _) = h
        .call(
            Method::PATCH,
            "/watchlist/api/lists/1",
            Some(json!({"name": "Renamed"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .call(Method::DELETE, "/watchlist/api/lists/1/items/2", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h
        .call(Method::DELETE, "/watchlist/api/lists/1/items/2", None)
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::NOT_FOUND, Some("Instrument not found"))
    );
    let (s, _) = h.post("/watchlist/api/lists/1/clear", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h.call(Method::DELETE, "/watchlist/api/lists/1", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h.call(Method::DELETE, "/watchlist/api/lists/1", None).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::NOT_FOUND, Some("List not found"))
    );
    let (_, v) = h.get("/watchlist/api/lists").await;
    assert_eq!(v, json!({"status": "success", "data": []}));
}

#[tokio::test]
async fn alert_log_records_lists_and_clears() {
    let h = H::new(true).await;
    let (s, v) = h.post("/alerts/fired", json!({"title": "x"})).await;
    assert_eq!(
        (s, v["message"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("That alert could not be identified")
        )
    );
    let (s, v) = h
        .post(
            "/alerts/fired",
            json!({"alertId": "a1", "title": "SBIN above 100", "price": 101.25,
            "symbol": "SBIN", "exchange": "NSE", "delivered": ["telegram"]}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        keys(&v["fire"]),
        set(&[
            "id",
            "alertId",
            "title",
            "kind",
            "condition",
            "symbol",
            "exchange",
            "interval",
            "price",
            "message",
            "delivered",
            "firedAt"
        ])
    );
    assert_eq!(v["fire"]["delivered"], json!(["telegram"]));
    assert_eq!(v["fire"]["firedAt"], json!(now().timestamp() as f64));
    h.post("/alerts/fired", json!({"alertId": "a2"})).await;
    let (_, v) = h.get("/alerts/log?limit=1").await;
    assert_eq!(v["fires"].as_array().unwrap().len(), 1);
    let (_, v) = h.get("/alerts/log").await;
    assert_eq!(v["fires"].as_array().unwrap().len(), 2);
    let (s, v) = h.call(Method::DELETE, "/alerts/log?alertId=a1", None).await;
    assert_eq!(
        (s, &v),
        (StatusCode::OK, &json!({"status": "success", "removed": 1}))
    );
    let (_, v) = h.call(Method::DELETE, "/alerts/log", None).await;
    assert_eq!(v["removed"], 1);
}

// ------------------------------------------------------------------ dashboard

#[tokio::test]
async fn dashboard_funds() {
    let h = H::new(false).await;
    let (s, v) = h.get("/auth/dashboard-data").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["code"], "BROKER_SESSION_EXPIRED");
    let h = H::new(true).await;
    let (s, v) = h.get("/auth/dashboard-data").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "success");
    assert_eq!(v["data"]["availablecash"], "125000.50");
    *h.mock.funds_ok.lock() = false;
    let (s, v) = h.get("/auth/dashboard-data").await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(v["status"], "error");
    h.analyze(true);
    let (s, v) = h.get("/auth/dashboard-data").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["availablecash"], 10000000.0);
}
