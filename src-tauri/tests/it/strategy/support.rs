#![allow(dead_code)]
use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::db::sqlite::SqliteDb;
use openalgo_desktop_lib::events::OrderUpdate;
use openalgo_desktop_lib::strategy::broadcast::RoomEmitter;
use openalgo_desktop_lib::strategy::dispatch::{
    Book, DispatchResult, OrderGateway, OrderPayload, OrderStatusResult, RunMode,
};
use openalgo_desktop_lib::strategy::engine::FillOpts;
use openalgo_desktop_lib::strategy::{Deps, StrategyModule};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const USER: &str = "trader";
pub const ATM_CE: &str = "NIFTY13OCT2624500CE";

pub fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Kolkata
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

fn row(
    symbol: &str,
    exchange: &str,
    name: &str,
    lot: i32,
    expiry: &str,
    strike: f64,
    it: &str,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: format!("{}:{}", exchange, symbol),
        expiry: expiry.into(),
        strike,
        lot_size: lot,
        instrument_type: it.into(),
        tick_size: 0.05,
    }
}

/// A small master: NIFTY index options (weekly and monthly), NIFTY
/// futures, RELIANCE cash and futures, SBIN cash.
pub fn master() -> Vec<SymToken> {
    let mut rows = vec![
        row("NIFTY", "NSE_INDEX", "NIFTY", 1, "", 0.0, "INDEX"),
        row("RELIANCE", "NSE", "RELIANCE", 1, "", 0.0, "EQ"),
        row("SBIN", "NSE", "SBIN", 1, "", 0.0, "EQ"),
        row(
            "NIFTY27OCT26FUT",
            "NFO",
            "NIFTY",
            65,
            "27-OCT-26",
            0.0,
            "FUT",
        ),
        row(
            "RELIANCE27OCT26FUT",
            "NFO",
            "RELIANCE",
            500,
            "27-OCT-26",
            0.0,
            "FUT",
        ),
    ];
    for (expiry, compact) in [("13-OCT-26", "13OCT26"), ("27-OCT-26", "27OCT26")] {
        for i in 0..=20 {
            let k = 24000.0 + 50.0 * i as f64;
            for t in ["CE", "PE"] {
                rows.push(row(
                    &format!("NIFTY{}{}{}", compact, k as i64, t),
                    "NFO",
                    "NIFTY",
                    65,
                    expiry,
                    k,
                    t,
                ));
            }
        }
    }
    rows
}

/// Holds a price request until the test lets it go, so a test can act
/// while a start is waiting on its legs.
pub struct Gate {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

impl Gate {
    /// Wait (bounded) until a request is held at the gate.
    pub async fn entered(&self) {
        let permit = tokio::time::timeout(Duration::from_secs(10), self.entered.acquire())
            .await
            .expect("nothing reached the gate")
            .unwrap();
        permit.forget();
    }

    /// Let one held request through.
    pub fn release(&self) {
        self.release.add_permits(1);
    }
}

/// The outside world, scripted.
#[derive(Default)]
pub struct FakeGateway {
    pub placed: Mutex<Vec<(RunMode, OrderPayload)>>,
    pub script: Mutex<VecDeque<DispatchResult>>,
    pub unauthorised: AtomicBool,
    pub ltps: Mutex<HashMap<(String, String), f64>>,
    pub statuses: Mutex<HashMap<String, Value>>,
    pub cancels: Mutex<Vec<String>>,
    pub delay_ms: AtomicU64,
    /// When set, every price request waits here.
    pub ltp_gate: Mutex<Option<Arc<Gate>>>,
    n: AtomicU64,
}

impl FakeGateway {
    /// Hold price requests at a new gate from now on.
    pub fn gate_ltp(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *self.ltp_gate.lock() = Some(gate.clone());
        gate
    }
}

impl FakeGateway {
    pub fn reject_next(&self, message: &str) {
        self.script.lock().push_back(DispatchResult {
            ok: false,
            broker_order_id: None,
            response: json!({"status": "error", "message": message}),
            error: Some(message.into()),
        });
    }

    pub fn placed(&self) -> Vec<OrderPayload> {
        self.placed.lock().iter().map(|(_, o)| o.clone()).collect()
    }

    pub fn actions(&self) -> Vec<String> {
        self.placed().into_iter().map(|o| o.action).collect()
    }
}

#[async_trait]
impl OrderGateway for FakeGateway {
    async fn place(&self, mode: RunMode, order: &OrderPayload) -> DispatchResult {
        self.placed.lock().push((mode, order.clone()));
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if let Some(r) = self.script.lock().pop_front() {
            return r;
        }
        let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
        DispatchResult {
            ok: true,
            broker_order_id: Some(format!("SB-{}", n)),
            response: json!({"status": "success", "orderid": format!("SB-{}", n)}),
            error: None,
        }
    }

    async fn cancel(&self, _mode: RunMode, id: &str) -> DispatchResult {
        self.cancels.lock().push(id.to_string());
        DispatchResult {
            ok: true,
            broker_order_id: Some(id.into()),
            ..Default::default()
        }
    }

    async fn order_status(&self, _mode: RunMode, id: &str) -> OrderStatusResult {
        match self.statuses.lock().get(id) {
            Some(v) => OrderStatusResult {
                ok: true,
                order: v.clone(),
                error: None,
            },
            None => OrderStatusResult {
                ok: false,
                order: Value::Null,
                error: Some("unknown".into()),
            },
        }
    }

    fn authorised(&self, mode: RunMode) -> Result<(), String> {
        if mode == RunMode::Live && self.unauthorised.load(Ordering::SeqCst) {
            return Err("Broker session is not available or has expired. Log in to your broker to restore it.".into());
        }
        Ok(())
    }

    fn broker_name(&self, mode: RunMode) -> String {
        match mode {
            RunMode::Sandbox => "sandbox".into(),
            RunMode::Live => "zerodha".into(),
        }
    }

    async fn book(&self, _mode: RunMode, _book: Book) -> Result<Value, Value> {
        Ok(json!({"status": "success", "data": []}))
    }

    async fn ltp(&self, symbol: &str, exchange: &str) -> Result<f64, String> {
        let gate = self.ltp_gate.lock().clone();
        if let Some(gate) = gate {
            gate.entered.add_permits(1);
            if let Ok(p) = gate.release.acquire().await {
                p.forget();
            }
        }
        self.ltps
            .lock()
            .get(&(symbol.to_string(), exchange.to_string()))
            .copied()
            .ok_or_else(|| "no price".to_string())
    }
}

/// Records every room frame; `watching` controls the subscriber check.
#[derive(Default)]
pub struct Rooms {
    pub frames: Mutex<Vec<(String, String, Value)>>,
    pub watching: AtomicBool,
}

impl Rooms {
    pub fn events(&self) -> Vec<String> {
        self.frames.lock().iter().map(|f| f.1.clone()).collect()
    }
}

#[async_trait]
impl RoomEmitter for Rooms {
    fn has_subscribers(&self, _room: &str) -> bool {
        self.watching.load(Ordering::SeqCst)
    }
    async fn emit_to(&self, room: &str, event: &str, payload: Value) {
        self.frames
            .lock()
            .push((room.to_string(), event.to_string(), payload));
    }
}

pub struct T {
    pub m: Arc<StrategyModule>,
    pub gw: Arc<FakeGateway>,
    pub rooms: Arc<Rooms>,
    pub clock: Arc<ManualClock>,
    pub symbols: SymbolResolver,
    /// The module's database, for fault injection.
    pub db: Arc<SqliteDb>,
    _dir: tempfile::TempDir,
}

pub fn t() -> T {
    t_at(ist(2026, 10, 7, 10, 0))
}

pub fn t_at(now: DateTime<Utc>) -> T {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(SqliteDb::new(&dir.path().join("openalgo.db")).unwrap());
    let clock = ManualClock::new(now);
    let gw = Arc::new(FakeGateway::default());
    gw.ltps
        .lock()
        .insert(("NIFTY".into(), "NSE_INDEX".into()), 24510.0);
    let rooms = Arc::new(Rooms::default());
    let symbols = SymbolResolver::new();
    symbols.load(master());
    let m = StrategyModule::new(Deps {
        db: db.clone(),
        gateway: gw.clone(),
        rooms: rooms.clone(),
        clock: clock.clone(),
        symbols: symbols.clone(),
        session_hour: 3,
        session_minute: 0,
        prices: None,
    });
    T {
        m,
        gw,
        rooms,
        clock,
        symbols,
        db,
        _dir: dir,
    }
}

/// The web suite's default batch leg: one short ATM call, 20 point stop.
pub fn short_call_leg() -> Value {
    json!({
        "id": 1, "segment": "options", "expiry": "weekly", "lots": 1,
        "position": "S", "option_type": "CE", "strike_mode": "atm",
        "atm_offset": "ATM", "sl_pts": 20, "trail": {"x": 0, "y": 0},
    })
}

pub fn config(name: &str, legs: Value, overrides: Value) -> Value {
    let mut c = json!({
        "name": name,
        "underlying": "NIFTY",
        "underlying_exchange": "NSE_INDEX",
        "universe_tab": "weekly_monthly",
        "product": "NRML",
        "strategy_type": "positional",
        "legs": legs,
    });
    if let Value::Object(o) = overrides {
        for (k, v) in o {
            c[k] = v;
        }
    }
    c
}

pub fn signal_config(legs: Value, overrides: Value) -> Value {
    let mut c = config(
        "Signal test",
        legs,
        json!({
            "strategy_kind": "signal", "universe_tab": "stocks_fno",
            "underlying": "RELIANCE", "underlying_exchange": "NSE", "product": "MIS",
        }),
    );
    if let Value::Object(o) = overrides {
        for (k, v) in o {
            c[k] = v;
        }
    }
    c
}

pub fn signal_leg(id: i64, symbol: &str, side: &str) -> Value {
    json!({"id": id, "symbol": symbol, "exchange": "NSE", "side": side, "qty": 10,
           "qty_mode": "units", "segment": "cash", "sl_pts": 20, "risk_unit": "points"})
}

impl T {
    /// Create directly through the store, as the web suite does.
    pub fn make(&self, cfg: Value) -> i64 {
        self.make_with_token(cfg).0
    }

    pub fn make_with_token(&self, cfg: Value) -> (i64, String) {
        let (row, token) = self.m.store.create_strategy(USER, &cfg).unwrap();
        (row.id, token)
    }

    pub fn default_strategy(&self) -> i64 {
        self.make(config("Engine test", json!([short_call_leg()]), json!({})))
    }

    /// The strategy as stored now.
    pub fn row(&self, sid: i64) -> openalgo_desktop_lib::strategy::store::StrategyRow {
        self.m.store.get_strategy(sid, USER).unwrap().unwrap()
    }

    /// A manual Sandbox start's claim at the strategy's current revision.
    pub fn claim(&self, sid: i64) -> openalgo_desktop_lib::strategy::store::ClaimOutcome {
        let claim = openalgo_desktop_lib::strategy::store::StartClaim {
            revision: self.row(sid).revision,
            live: false,
            webhook: false,
        };
        self.m.store.claim_strategy_for_run(sid, claim).unwrap()
    }

    pub async fn start(&self, sid: i64) -> openalgo_desktop_lib::strategy::StartResult {
        self.m.start_run(sid, USER, "sandbox", "manual", None).await
    }

    pub async fn start_filled(&self, sid: i64, price: f64) -> i64 {
        let r = self.start(sid).await;
        assert!(r.ok, "{:?}", r);
        let run = r.run_id.unwrap();
        for leg in self.m.state.snapshot(run).unwrap().legs.values() {
            self.m
                .apply_fill(run, leg.leg_id, Some(price), true, FillOpts::default())
                .await;
        }
        run
    }

    pub fn leg(&self, run: i64, leg: i64) -> openalgo_desktop_lib::strategy::state::LegState {
        self.m
            .state
            .snapshot(run)
            .unwrap()
            .leg(leg)
            .unwrap()
            .clone()
    }

    pub fn orders(&self, run: i64) -> Vec<openalgo_desktop_lib::strategy::store::OrderRow> {
        self.m.store.list_orders(run).unwrap()
    }

    pub fn run(&self, run: i64) -> openalgo_desktop_lib::strategy::store::RunRow {
        self.m.store.get_run(run).unwrap().unwrap()
    }

    pub fn events(&self, sid: i64) -> Vec<Value> {
        self.m
            .store
            .list_events(sid, None, None, None, 1000)
            .unwrap()
    }

    pub fn event_kinds(&self, sid: i64) -> Vec<String> {
        self.events(sid)
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    }

    /// One broker frame for an order, through the order-update path.
    pub async fn frame(&self, broker_id: &str, status: &str, filled: i64, price: f64) {
        self.m
            .apply_update(
                broker_id,
                OrderUpdate {
                    orderid: broker_id.into(),
                    order_status: status.into(),
                    filled_quantity: filled,
                    average_price: price,
                    ..Default::default()
                },
            )
            .await;
    }

    /// Fill the exit row of a leg (the newest exit order).
    pub async fn fill_last_exit(&self, run: i64, price: f64) {
        let o = self
            .orders(run)
            .into_iter()
            .rev()
            .find(|o| o.kind != "entry" && o.broker_order_id.is_some())
            .unwrap();
        self.frame(
            o.broker_order_id.as_deref().unwrap(),
            "complete",
            o.qty,
            price,
        )
        .await;
    }
}

// ------------------------------------------------------------- HTTP

pub struct App {
    pub ctx: Arc<openalgo_desktop_lib::state::AppState>,
    pub clock: Arc<ManualClock>,
    pub key: String,
    pub cookie: String,
    pub csrf: String,
    pub mock: Arc<openalgo_desktop_lib::brokers::mock::MockBroker>,
    _dir: tempfile::TempDir,
}

/// A full app context: signed-in session, API key, mock broker connected
/// with quotes for every master row.
pub fn app() -> App {
    use openalgo_desktop_lib::brokers::mock::MockBroker;
    use openalgo_desktop_lib::brokers::types::Quote;
    use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
    use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
    use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
    use openalgo_desktop_lib::services::auth_service::AuthService;
    use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
    use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
    let dir = tempfile::tempdir().unwrap();
    let symbols = SymbolResolver::new();
    let mock = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
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
    let rows = master();
    for r in &rows {
        let ltp = match r.instrument_type.as_str() {
            "CE" | "PE" => 100.0,
            "FUT" => 24600.0,
            "INDEX" => 24510.0,
            _ => 1000.0,
        };
        mock.set_quote(Quote {
            symbol: r.symbol.clone(),
            exchange: r.exchange.clone(),
            ltp,
            close: ltp,
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
    let s = ctx.sessions.create(ctx.now());
    ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
    App {
        cookie: format!("session={}", s.id),
        csrf: s.csrf_token,
        ctx,
        clock,
        key,
        mock,
        _dir: dir,
    }
}

impl App {
    pub async fn send(&self, mut req: Request<Body>) -> (StatusCode, Value) {
        if req.extensions().get::<ConnectInfo<SocketAddr>>().is_none() {
            req.extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        }
        // Every real client names the host; a request over a connection
        // without one is refused (security review S-13).
        crate::with_host(&mut req, &self.ctx);
        let app = openalgo_desktop_lib::server::app(self.ctx.clone());
        use tower::ServiceExt;
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    pub fn req(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        csrf: bool,
    ) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::COOKIE, &self.cookie);
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

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(Method::GET, path, None, false)).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(self.req(Method::POST, path, Some(body), true))
            .await
    }

    pub async fn api(&self, path: &str, mut body: Value) -> (StatusCode, Value) {
        body["apikey"] = json!(self.key);
        self.send(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    pub async fn webhook(&self, token: &str, body: &str) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/strategy/webhook/{}", token))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    /// The public webhook, called from `ip`.
    pub async fn webhook_from(&self, ip: &str, token: &str, body: &str) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(format!("/strategy/webhook/{}", token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let addr: std::net::IpAddr = ip.parse().unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(addr, 40000)));
        self.send(req).await
    }

    /// Wait until `f` holds (the bus delivers on worker tasks).
    pub async fn until(&self, mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..400 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        f()
    }
}
