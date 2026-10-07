//! Strategy module and RMS: the risk vectors, ports of the web's
//! `test/test_strategy_module_*.py` suites (each module is named after its
//! web source file, each test after its web test), every PORTED DEFECT, the
//! barrier-synchronised invariants, webhook auth and payload validation, the
//! session and `/api/v1/strategy` routes, and resource hygiene.

mod support {
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
        n: AtomicU64,
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
            db,
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
}

use openalgo_desktop_lib::strategy::engine::FillOpts;
use serde_json::{json, Value};
use support::*;

// ===================================================================== vectors

mod risk_vectors {
    use openalgo_desktop_lib::risk::{
        evaluate_position, evaluate_trail, value_to_f64, PositionRisk,
    };
    use serde_json::Value;

    fn close(a: Option<f64>, e: &Value, tol: f64) -> bool {
        match (a, e) {
            (None, Value::Null) => true,
            (Some(a), e) => e.as_f64().map(|e| (a - e).abs() <= tol).unwrap_or(false),
            _ => false,
        }
    }

    /// Every case in the web's `test/risk/vectors.json`, through both entry
    /// points. The count is asserted so a dropped case cannot pass silently.
    #[test]
    fn every_case_in_vectors_json_passes() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/risk/vectors.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tol = v["tolerance"].as_f64().unwrap();
        let cases = v["cases"].as_array().unwrap();
        let mut passed = 0;
        for c in cases {
            let name = c["name"].as_str().unwrap();
            let d = evaluate_position(
                &PositionRisk::from_state(&c["state"]),
                value_to_f64(&c["ltp"]),
            );
            for (k, want) in c["expected"].as_object().unwrap() {
                let ok = match k.as_str() {
                    "reason" => d.reason.map(|r| r.as_str()) == want.as_str(),
                    "evaluated" => Some(d.evaluated) == want.as_bool(),
                    "breached" => Some(d.breached) == want.as_bool(),
                    "stop_moved" => Some(d.stop_moved) == want.as_bool(),
                    "trail_armed" => Some(d.trail_armed) == want.as_bool(),
                    "current_sl" => close(d.stop_price, want, tol),
                    "highest_price" => close(d.highest_price, want, tol),
                    "lowest_price" => close(d.lowest_price, want, tol),
                    "pnl" => close(Some(d.pnl), want, tol),
                    other => panic!("{}: unknown key {}", name, other),
                };
                assert!(ok, "{}: {} mismatched: {:?}", name, k, d);
            }
            let legacy = evaluate_trail(&c["state"], &c["ltp"]);
            assert_eq!(legacy["breached"], c["expected"]["breached"], "{}", name);
            assert_eq!(legacy["reason"], c["expected"]["reason"], "{}", name);
            passed += 1;
        }
        assert_eq!(passed, cases.len());
        assert_eq!(passed, 35, "35 golden vectors expected");
    }
}

// ===================================================================== risk adapter

/// web: test/test_strategy_module_risk.py
mod strategy_module_risk {
    use openalgo_desktop_lib::strategy::risk_adapter as ra;
    use openalgo_desktop_lib::strategy::state::{new_leg_state, LegSpec, LegState, RunState};
    use serde_json::json;

    fn leg(position: &str) -> LegState {
        let mut l = new_leg_state(&LegSpec {
            leg_id: 1,
            position: "B".into(),
            symbol: "X".into(),
            exchange: "NFO".into(),
            lots: 1,
            quantity: 10,
            risk_unit: "points".into(),
            ..Default::default()
        })
        .unwrap();
        l.position = position.into();
        l.status = "open".into();
        l.entry_status = "complete".into();
        l
    }

    fn priced(position: &str, entry: f64, qty: i64, ltp: Option<f64>) -> LegState {
        let mut l = leg(position);
        l.entry_avg = entry;
        l.qty = qty;
        l.ltp = ltp;
        l
    }

    fn state(legs: Vec<LegState>) -> RunState {
        let mut legs = legs;
        for (i, l) in legs.iter_mut().enumerate() {
            l.leg_id = i as i64 + 1;
        }
        RunState::new(1, 1, legs)
    }

    #[test]
    fn a_leg_without_a_usable_side_is_refused_not_defaulted() {
        // PORTED DEFECT. The original never writes `position` on signal legs
        // and reads anything not "B" as a short, so the stop fired on a
        // favourable move. Defaulting is the bug; refusing is the fix.
        for bad in ["", "LONG", "buy", "x"] {
            assert!(ra::leg_to_position_risk(&leg(bad)).is_err(), "{:?}", bad);
            assert!(new_leg_state(&LegSpec {
                position: bad.into(),
                ..Default::default()
            })
            .is_err());
        }
    }

    #[test]
    fn a_short_leg_is_evaluated_as_a_short() {
        let mut l = priced("S", 100.0, 10, None);
        l.sl_pts = Some(20.0);
        l.target_pts = Some(30.0);
        let r = ra::leg_to_position_risk(&l).unwrap();
        assert_eq!(r.stop_price, Some(120.0));
        assert_eq!(r.target_price, Some(70.0));
        let d = ra::evaluate_leg(&mut l, 121.0).unwrap();
        assert!(d.breached && d.reason.unwrap().as_str() == "sl" && d.pnl < 0.0);
    }

    #[test]
    fn a_long_leg_is_evaluated_as_a_long() {
        let mut l = priced("B", 100.0, 10, None);
        l.sl_pts = Some(20.0);
        l.target_pts = Some(30.0);
        let r = ra::leg_to_position_risk(&l).unwrap();
        assert_eq!(r.stop_price, Some(80.0));
        assert_eq!(r.target_price, Some(130.0));
        assert_eq!(
            ra::evaluate_leg(&mut l.clone(), 79.0)
                .unwrap()
                .reason
                .unwrap()
                .as_str(),
            "sl"
        );
        let d = ra::evaluate_leg(&mut l, 131.0).unwrap();
        assert_eq!(d.reason.unwrap().as_str(), "target");
        assert!(d.pnl > 0.0);
    }

    #[test]
    fn a_fixed_distance_trail_uses_x_as_its_gap() {
        let mut l = priced("B", 100.0, 10, None);
        l.sl_pts = Some(20.0);
        l.trail_x = 5.0;
        let d = ra::evaluate_leg(&mut l, 110.0).unwrap();
        assert!(d.trail_armed);
        assert_eq!(d.stop_price, Some(105.0));
    }

    #[test]
    fn a_percent_leg_converts_against_its_own_entry() {
        let mut l = priced("S", 2500.0, 10, None);
        l.risk_unit = "percent".into();
        l.sl_pts = Some(2.0);
        l.target_pts = Some(4.0);
        let r = ra::leg_to_position_risk(&l).unwrap();
        assert_eq!(r.stop_price, Some(2550.0));
        assert_eq!(r.target_price, Some(2400.0));
    }

    #[test]
    fn a_percent_leg_with_no_confirmed_fill_gets_no_levels() {
        let mut l = priced("B", 0.0, 10, None);
        l.risk_unit = "percent".into();
        l.sl_pts = Some(2.0);
        let r = ra::leg_to_position_risk(&l).unwrap();
        assert_eq!(r.stop_price, None);
        assert_eq!(r.target_price, None);
    }

    #[test]
    fn run_pnl_marks_from_entry_rather_than_a_stale_leg_field() {
        // PORTED DEFECT. The original summed a per-leg `mtm` written on an
        // earlier pass, so a stale field poisoned every strategy-level rule.
        let mut l = priced("B", 100.0, 10, Some(110.0));
        l.mtm = -99999.0;
        let (realized, unrealized) = ra::run_pnl(&state(vec![l])).unwrap();
        assert_eq!(realized, 0.0);
        assert!((unrealized - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_closed_leg_contributes_its_realized_figure() {
        let open = priced("B", 100.0, 10, Some(105.0));
        let mut closed = priced("B", 100.0, 10, None);
        closed.status = "closed".into();
        closed.realized_pnl = 250.0;
        let (r, u) = ra::run_pnl(&state(vec![open, closed])).unwrap();
        assert!((r - 250.0).abs() < 1e-9);
        assert!((u - 50.0).abs() < 1e-9);
    }

    #[test]
    fn a_reentered_signal_leg_keeps_its_earlier_round_trip() {
        let mut l = priced("B", 100.0, 10, Some(101.0));
        l.realized_pnl = -500.0;
        let (r, _) = ra::run_pnl(&state(vec![l])).unwrap();
        assert!((r + 500.0).abs() < 1e-9);
    }

    #[test]
    fn peak_and_trough_are_written_on_every_pass_not_only_on_a_breach() {
        // PORTED DEFECT. The original persisted peak and trough on one of
        // several stop paths only.
        let mut s = state(vec![priced("B", 100.0, 10, Some(120.0))]);
        let strategy = json!({"overall_sl_mtm": null, "overall_target_mtm": null});
        ra::evaluate_run(&mut s, &strategy).unwrap();
        assert!((s.pnl_peak - 200.0).abs() < 1e-9);
        s.legs.get_mut("1").unwrap().ltp = Some(90.0);
        ra::evaluate_run(&mut s, &strategy).unwrap();
        assert!((s.pnl_peak - 200.0).abs() < 1e-9);
        assert!((s.pnl_trough + 100.0).abs() < 1e-9);
        assert!((s.pnl_total + 100.0).abs() < 1e-9);
    }

    #[test]
    fn the_overall_stop_is_entered_positive_and_applied_negative() {
        let mut s = state(vec![priced("B", 100.0, 10, Some(60.0))]);
        let d = ra::evaluate_run(&mut s, &json!({"overall_sl_mtm": 300})).unwrap();
        assert_eq!(d.reason.unwrap().as_str(), "combined_sl");
    }

    #[test]
    fn the_overall_target_fires_on_the_total() {
        let mut s = state(vec![priced("B", 100.0, 10, Some(160.0))]);
        let d = ra::evaluate_run(&mut s, &json!({"overall_target_mtm": 500})).unwrap();
        assert_eq!(d.reason.unwrap().as_str(), "combined_target");
    }

    #[test]
    fn a_plain_lock_does_not_trail_even_with_a_step_configured() {
        let mut s = state(vec![priced("B", 100.0, 10, Some(160.0))]);
        let d = ra::evaluate_run(
            &mut s,
            &json!({"lock_profit": {"mode": "lock", "if_profit_reaches": 500,
                "lock_profit": 200, "trail_step": 50}}),
        )
        .unwrap();
        assert!(d.lock_armed);
        assert_eq!(d.lock_floor, Some(200.0));
        let d = ra::evaluate_run(
            &mut s,
            &json!({"lock_profit": {"mode": "lock_and_trail", "if_profit_reaches": 500,
                "lock_profit": 200, "trail_step": 50}}),
        )
        .unwrap();
        assert_eq!(d.lock_floor, Some(550.0));
    }

    #[test]
    fn trail_to_entry_moves_the_other_open_legs() {
        let mut a = priced("B", 100.0, 10, Some(110.0));
        a.sl_pts = Some(20.0);
        let mut b = priced("B", 50.0, 10, Some(60.0));
        b.sl_pts = Some(10.0);
        let mut s = state(vec![a, b]);
        let moved = ra::trail_open_legs_to_entry(&mut s, 1);
        assert_eq!(moved, vec!["2".to_string()]);
        assert_eq!(s.legs["2"].effective_sl, Some(50.0));
        assert!(s.trail_to_entry_active);
    }
}
// ===================================================================== dispatch

/// web: test/test_strategy_module_order_dispatch.py
mod strategy_module_order_dispatch {
    use openalgo_desktop_lib::strategy::dispatch::{
        build_order, exit_action, product_for_exchange,
    };

    #[test]
    fn the_exit_action_is_the_opposite_of_the_held_side() {
        assert_eq!(exit_action("B").unwrap(), "SELL");
        assert_eq!(exit_action("S").unwrap(), "BUY");
        assert_eq!(exit_action("b").unwrap(), "SELL");
    }

    #[test]
    fn an_exit_refuses_to_guess_a_side() {
        // PORTED DEFECT. The original derived the exit action from the leg's
        // configured side, which defaulted to "B", so an exit on a short
        // placed another SELL and doubled the position. Refusing beats
        // defaulting.
        for bad in ["", "LONG", "SHORT", "x"] {
            assert!(exit_action(bad).is_err(), "{:?}", bad);
        }
    }

    #[test]
    fn the_product_is_translated_to_the_venue() {
        assert_eq!(product_for_exchange("MIS", "NFO"), "MIS");
        assert_eq!(product_for_exchange("MIS", "NSE"), "MIS");
        assert_eq!(product_for_exchange("NRML", "NSE"), "CNC");
        assert_eq!(product_for_exchange("CNC", "NFO"), "NRML");
        assert_eq!(product_for_exchange("NRML", "MCX"), "NRML");
    }

    #[test]
    fn an_order_is_tagged_with_the_strategy_and_uppercased() {
        let o = build_order(
            "NIFTY13OCT2624500CE",
            "NFO",
            "sell",
            65,
            "NRML",
            "Iron condor",
            "MARKET",
        );
        assert_eq!(o.action, "SELL");
        assert_eq!(o.strategy, "Iron condor");
        assert_eq!(o.pricetype, "MARKET");
        let r = o.to_request();
        assert_eq!(r["quantity"], 65);
        assert_eq!(r["product"], "NRML");
    }
}

// ===================================================================== state

/// web: services/strategy_module/state.py claims (test_strategy_module_engine.py,
/// test_strategy_module_qa_edges.py)
mod strategy_module_state {
    use openalgo_desktop_lib::strategy::state::{
        new_leg_state, ClaimId, EntryDecision, LegSpec, RunState, StateRegistry,
    };

    fn registry_with_open_leg(entry_status: &str) -> StateRegistry {
        let reg = StateRegistry::new();
        let mut l = new_leg_state(&LegSpec {
            leg_id: 1,
            position: "S".into(),
            symbol: "X".into(),
            exchange: "NFO".into(),
            quantity: 75,
            position_ref: Some("ref1".into()),
            ..Default::default()
        })
        .unwrap();
        l.status = "open".into();
        l.entry_status = entry_status.into();
        reg.install(RunState::new(7, 1, vec![l]));
        reg
    }

    #[test]
    fn a_second_claim_on_one_leg_is_refused() {
        let reg = registry_with_open_leg("complete");
        assert!(reg.claim_leg_exit(7, 1, "exit_sl").is_some());
        assert!(reg.claim_leg_exit(7, 1, "exit_target").is_none());
    }

    #[test]
    fn the_claim_marker_is_written_before_any_order_id() {
        let reg = registry_with_open_leg("complete");
        reg.claim_leg_exit(7, 1, "exit_sl").unwrap();
        let leg = reg.snapshot(7).unwrap().leg(1).unwrap().clone();
        assert_eq!(leg.exit_kind.as_deref(), Some("exit_sl"));
        assert!(leg.exit_order_id.is_none());
    }

    #[test]
    fn an_accepted_but_unfilled_entry_is_never_claimed_for_exit() {
        let reg = registry_with_open_leg("open");
        assert!(reg.claim_leg_exit(7, 1, "exit_sl").is_none());
        let (claimed, unfilled) = reg.claim_legs_for_exit(7, &[1], "exit_close_all");
        assert!(claimed.is_empty());
        assert_eq!(unfilled.len(), 1);
    }

    #[test]
    fn a_released_claim_makes_the_leg_exitable_again() {
        let reg = registry_with_open_leg("complete");
        let c = reg.claim_leg_exit(7, 1, "exit_sl").unwrap();
        assert!(!reg.release_leg_exit(7, 1, &ClaimId::Token("someone-else".into())));
        assert!(reg.release_leg_exit(7, 1, &ClaimId::Token(c.claim_token)));
        assert!(reg.claim_leg_exit(7, 1, "exit_target").is_some());
    }

    #[test]
    fn a_claim_on_a_cleared_run_registers_nothing() {
        let reg = StateRegistry::new();
        assert!(reg.claim_leg_exit(99, 1, "exit_sl").is_none());
        assert!(
            reg.is_empty(),
            "probing an unknown run must not create state"
        );
    }

    #[test]
    fn clearing_a_run_drops_its_state_and_lock() {
        let reg = registry_with_open_leg("complete");
        reg.clear(7);
        assert!(reg.snapshot(7).is_none());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn a_stopping_run_refuses_new_signal_entries() {
        let reg = registry_with_open_leg("complete");
        reg.mark_stopping(7);
        assert_eq!(
            reg.claim_signal_entry(7, 2, "B"),
            Some(EntryDecision::Note("run_stopping"))
        );
    }

    #[test]
    fn a_repeated_entry_on_the_held_side_is_a_noop() {
        let reg = registry_with_open_leg("complete");
        assert_eq!(
            reg.claim_signal_entry(7, 1, "S"),
            Some(EntryDecision::Note("already_short"))
        );
    }

    #[test]
    fn one_signal_entry_decision_per_leg_at_a_time() {
        let reg = registry_with_open_leg("complete");
        assert!(matches!(
            reg.claim_signal_entry(7, 3, "B"),
            Some(EntryDecision::Claimed(_))
        ));
        assert_eq!(
            reg.claim_signal_entry(7, 3, "B"),
            Some(EntryDecision::Note("flip_pending"))
        );
    }

    #[test]
    fn the_checkpoint_snapshot_round_trips() {
        let reg = registry_with_open_leg("complete");
        let s = reg.snapshot(7).unwrap();
        let snap = s.snapshot_for_checkpoint();
        let legs: std::collections::BTreeMap<
            String,
            openalgo_desktop_lib::strategy::state::LegState,
        > = serde_json::from_value(snap["leg_state"].clone()).unwrap();
        assert_eq!(legs, s.legs);
        assert_eq!(snap["leg_state"]["1"]["position"], "S");
    }
}
// ===================================================================== engine

/// web: test/test_strategy_module_engine.py
mod strategy_module_engine {
    use super::*;

    #[tokio::test]
    async fn a_leg_that_cannot_be_resolved_stops_the_start_before_anything_is_claimed() {
        let t = t();
        let mut leg = short_call_leg();
        leg["atm_offset"] = json!("OTM5");
        leg["option_type"] = json!("XX");
        let sid = t.make(config("Bad leg", json!([leg]), json!({})));
        let r = t.start(sid).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().starts_with("Leg 1:"));
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        assert_eq!(s.status, "stopped");
        assert!(t.m.store.list_runs(sid, 10).unwrap().is_empty());
        assert!(t.gw.placed().is_empty());
    }

    #[tokio::test]
    async fn a_second_start_is_refused_by_the_atomic_claim() {
        let t = t();
        let sid = t.default_strategy();
        assert!(t.start(sid).await.ok);
        let r = t.start(sid).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("already running"));
        assert_eq!(t.gw.placed().len(), 1);
    }

    #[tokio::test]
    async fn live_is_refused_unless_the_strategy_opted_in() {
        let t = t();
        let sid = t.default_strategy();
        let r = t.m.start_run(sid, USER, "live", "manual", None).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("not enabled for live trading"));
        assert!(t.gw.placed().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_mode_is_refused() {
        let t = t();
        let sid = t.default_strategy();
        let r = t.m.start_run(sid, USER, "paper", "manual", None).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("Unknown run mode"));
    }

    #[tokio::test]
    async fn a_signal_strategy_has_no_start() {
        let t = t();
        let sid = t.make(signal_config(
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({}),
        ));
        let r = t.start(sid).await;
        assert!(!r.ok && r.error.unwrap().contains("signal strategy has no start"));
    }

    #[tokio::test]
    async fn entries_are_placed_longs_first() {
        let t = t();
        let mut buy = short_call_leg();
        buy["id"] = json!(2);
        buy["position"] = json!("B");
        buy["atm_offset"] = json!("OTM2");
        let sid = t.make(config("Spread", json!([short_call_leg(), buy]), json!({})));
        assert!(t.start(sid).await.ok);
        assert_eq!(t.gw.actions(), vec!["BUY", "SELL"]);
    }

    #[tokio::test]
    async fn every_leg_of_one_spread_is_priced_off_the_same_quote() {
        let t = t();
        let mut b = short_call_leg();
        b["id"] = json!(2);
        let sid = t.make(config("Straddle", json!([short_call_leg(), b]), json!({})));
        let r = t.start(sid).await;
        assert!(r.ok);
        let symbols: Vec<String> = t.gw.placed().into_iter().map(|o| o.symbol).collect();
        assert_eq!(symbols, vec![ATM_CE, ATM_CE]);
    }

    #[tokio::test]
    async fn every_entry_rejected_finalises_the_run_rather_than_leaving_it_running() {
        let t = t();
        let sid = t.default_strategy();
        t.gw.reject_next("Insufficient funds");
        let r = t.start(sid).await;
        assert!(!r.ok);
        assert_eq!(
            r.error.as_deref(),
            Some("Every entry order was rejected: Insufficient funds")
        );
        let run = t.run(r.run_id.unwrap());
        assert!(run.stopped_at.is_some());
        assert_eq!(run.stop_reason.as_deref(), Some("error"));
        assert_eq!(
            t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
            "stopped"
        );
        assert!(t.m.state.is_empty());
    }

    #[tokio::test]
    async fn a_started_run_records_its_orders_and_live_state() {
        let t = t();
        let sid = t.default_strategy();
        let r = t.start(sid).await;
        let run = r.run_id.unwrap();
        let orders = t.orders(run);
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].kind, "entry");
        assert_eq!(orders[0].status, "open");
        assert_eq!(orders[0].broker_order_id.as_deref(), Some("SB-1"));
        assert_eq!(orders[0].position_ref.as_ref().map(|p| p.len()), Some(32));
        let leg = t.leg(run, 1);
        assert_eq!(leg.position, "S");
        assert_eq!(leg.symbol, ATM_CE);
        assert_eq!(leg.qty, 65);
        assert_eq!(leg.entry_order_id, Some(orders[0].id));
        assert_eq!(leg.position_ref, orders[0].position_ref);
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        assert_eq!(
            (s.status.as_str(), s.current_run_id),
            ("running", Some(run))
        );
    }

    #[tokio::test]
    async fn an_entry_fill_sets_the_price_risk_is_measured_from() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        let leg = t.leg(run, 1);
        assert_eq!(leg.entry_avg, 100.0);
        assert_eq!(leg.entry_status, "complete");
        assert_eq!(leg.status, "open");
    }

    #[tokio::test]
    async fn an_exit_fill_locks_in_realized_pnl_with_the_right_sign() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 90.0).await;
        let r = t.run(run);
        assert!(r.stopped_at.is_some());
        // A short from 100 covered at 90 made 10 x 65.
        assert!((r.pnl_realized - 650.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn a_long_exit_fill_carries_the_opposite_sign() {
        let t = t();
        let mut leg = short_call_leg();
        leg["position"] = json!("B");
        let sid = t.make(config("Long", json!([leg]), json!({})));
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 90.0).await;
        assert!((t.run(run).pnl_realized + 650.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn an_exit_uses_the_symbol_the_run_holds_not_a_re_resolved_one() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        // The underlying moves 400 points: a re-resolved ATM would differ.
        t.gw.ltps
            .lock()
            .insert(("NIFTY".into(), "NSE_INDEX".into()), 24910.0);
        t.m.stop_run(run, USER, "manual").await;
        let placed = t.gw.placed();
        assert_eq!(placed.last().unwrap().symbol, ATM_CE);
    }

    #[tokio::test]
    async fn an_exit_covers_a_short_rather_than_adding_to_it() {
        // PORTED DEFECT. The original derived the exit action from the
        // configured side, which defaulted to "B", so an exit on a short
        // placed another SELL and doubled the position.
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    }

    #[tokio::test]
    async fn a_leg_already_exiting_is_not_sent_a_second_exit() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        // A stop-loss tick, then the operator's close: one exit.
        t.m.process_tick(ATM_CE, "NFO", 121.0).await;
        let r = t.m.close_leg(run, 1, USER).await;
        assert!(!r.ok);
        let exits = t
            .orders(run)
            .into_iter()
            .filter(|o| o.kind != "entry")
            .count();
        assert_eq!(exits, 1);
        assert_eq!(t.gw.placed().len(), 2);
    }

    #[tokio::test]
    async fn a_rejected_exit_can_be_retried_rather_than_looking_like_a_duplicate() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.gw.reject_next("RMS: margin exceeded");
        let r = t.m.close_leg(run, 1, USER).await;
        assert!(!r.ok);
        let leg = t.leg(run, 1);
        assert!(
            leg.exit_kind.is_none()
                && leg.exit_claim_token.is_none()
                && leg.exit_order_id.is_none()
        );
        let r = t.m.close_leg(run, 1, USER).await;
        assert!(r.ok, "{:?}", r);
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
    }

    #[tokio::test]
    async fn a_manual_close_does_not_trail_the_other_legs_to_entry() {
        let t = t();
        let mut b = short_call_leg();
        b["id"] = json!(2);
        b["atm_offset"] = json!("OTM1");
        let sid = t.make(config(
            "Two",
            json!([short_call_leg(), b]),
            json!({"trail_sl_to_entry": true}),
        ));
        let run = t.start_filled(sid, 100.0).await;
        t.m.close_leg(run, 1, USER).await;
        let s = t.m.state.snapshot(run).unwrap();
        assert!(!s.trail_to_entry_active);
        assert!(s.leg(2).unwrap().effective_sl.is_none());
    }

    #[tokio::test]
    async fn the_run_finalises_when_the_last_exit_fills_not_when_it_is_placed() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        let r = t.m.stop_run(run, USER, "manual").await;
        assert!(r.ok && r.stop_pending);
        assert!(t.run(run).stopped_at.is_none());
        assert_eq!(t.leg(run, 1).status, "open");
        t.fill_last_exit(run, 95.0).await;
        assert!(t.run(run).stopped_at.is_some());
    }

    #[tokio::test]
    async fn a_stop_loss_tick_exits_that_leg() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 120.0).await;
        let exit = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind != "entry")
            .unwrap();
        assert_eq!(
            (exit.kind.as_str(), exit.action.as_str()),
            ("exit_sl", "BUY")
        );
        assert!(t.event_kinds(sid).contains(&"leg_sl_hit".to_string()));
    }

    #[tokio::test]
    async fn a_quiet_tick_places_nothing() {
        let t = t();
        let sid = t.default_strategy();
        t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 105.0).await;
        assert_eq!(t.gw.placed().len(), 1);
    }

    #[tokio::test]
    async fn an_overall_stop_closes_the_whole_run() {
        let t = t();
        let sid = t.make(config(
            "SL",
            json!([short_call_leg()]),
            json!({"overall_sl_mtm": 500}),
        ));
        let run = t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 110.0).await; // -650
        let run_row = t.run(run);
        assert_eq!(run_row.stop_requested_reason.as_deref(), Some("overall_sl"));
        let exit = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind != "entry")
            .unwrap();
        assert_eq!(exit.kind, "exit_overall_sl");
    }

    #[tokio::test]
    async fn a_tick_for_an_instrument_no_run_holds_is_ignored() {
        let t = t();
        let sid = t.default_strategy();
        t.start_filled(sid, 100.0).await;
        t.m.process_tick("SOMETHING-ELSE", "NFO", 1.0).await;
        assert_eq!(t.gw.placed().len(), 1);
    }

    #[tokio::test]
    async fn peak_and_trough_reach_the_run_row_on_a_rule_driven_stop() {
        // PORTED DEFECT. The original passed peak and trough on only one of
        // its stop paths, so a rule-driven stop recorded both as zero.
        let t = t();
        let sid = t.make(config(
            "PT",
            json!([short_call_leg()]),
            json!({"overall_sl_mtm": 500}),
        ));
        let run = t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 95.0).await; // +325
        t.m.process_tick(ATM_CE, "NFO", 110.0).await; // -650, breaches
        let pending = t.run(run);
        assert_eq!(pending.stop_requested_reason.as_deref(), Some("overall_sl"));
        assert!(pending.stopped_at.is_none());
        t.fill_last_exit(run, 110.0).await;
        let done = t.run(run);
        assert!((done.pnl_peak - 325.0).abs() < 1e-6, "{}", done.pnl_peak);
        assert!(
            (done.pnl_trough + 650.0).abs() < 1e-6,
            "{}",
            done.pnl_trough
        );
        assert_eq!(done.stop_reason.as_deref(), Some("overall_sl"));
    }

    #[tokio::test]
    async fn a_finished_run_leaves_no_live_state_behind() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 100.0).await;
        assert!(t.m.state.snapshot(run).is_none());
        assert!(t.m.state.is_empty());
        assert_eq!(t.m.feed.runs_tracked(), 0);
    }

    #[tokio::test]
    async fn a_flat_run_can_finalize_without_a_broker_session() {
        let t = t();
        let sid = t.default_strategy();
        let r = t.start(sid).await;
        let run = r.run_id.unwrap();
        // The entry dies at the broker, so nothing is held.
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "rejected", 0, 0.0).await;
        t.gw.unauthorised
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let out = t.m.stop_run(run, USER, "manual").await;
        assert!(out.ok && !out.stop_pending);
        assert!(t.run(run).stopped_at.is_some());
    }

    #[tokio::test]
    async fn a_stop_whose_exits_were_refused_leaves_the_run_open_and_managed() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.gw.reject_next("Exchange closed");
        let r = t.m.stop_run(run, USER, "manual").await;
        assert!(!r.ok && r.stop_pending);
        assert!(r
            .error
            .unwrap()
            .contains("1 of 1 exit order(s) were refused"));
        let row = t.run(run);
        assert!(row.stopped_at.is_none());
        assert!(
            t.m.state.snapshot(run).is_some(),
            "the run is still managed"
        );
        assert_eq!(
            t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
            "running"
        );
        assert!(t.event_kinds(sid).contains(&"run_stop_failed".to_string()));
        // The stop can be retried and succeeds.
        let r = t.m.stop_run(run, USER, "manual").await;
        assert!(r.ok && r.stop_pending);
        t.fill_last_exit(run, 99.0).await;
        assert!(t.run(run).stopped_at.is_some());
    }

    #[tokio::test]
    async fn a_stop_of_an_unfilled_entry_is_refused_and_retried_once_it_fills() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let r = t.m.stop_run(run, USER, "manual").await;
        // The cancel was sent; nothing was exited against an unfilled entry.
        assert_eq!(t.gw.cancels.lock().len(), 1);
        assert!(!r.ok && r.stop_pending, "{:?}", r);
        assert_eq!(t.gw.actions(), vec!["SELL"]);
        // The entry fills after all: the durable stop exits it.
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "complete", 65, 100.0).await;
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    }

    #[tokio::test]
    async fn a_keyless_stop_with_possible_exposure_is_durable_pending_and_retryable() {
        let t = t();
        let sid = t.default_strategy();
        let run =
            t.m.start_run(sid, USER, "sandbox", "manual", None)
                .await
                .run_id
                .unwrap();
        t.m.apply_fill(run, 1, Some(100.0), true, FillOpts::default())
            .await;
        // Mark the run live after the fact so the broker session matters.
        t.m.store
            .execute_raw(&format!(
                "UPDATE sm_strategy_run SET mode = 'live' WHERE id = {}",
                run
            ))
            .unwrap();
        t.gw.unauthorised
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let r = t.m.stop_run(run, USER, "manual").await;
        assert!(!r.ok && r.stop_pending);
        assert_eq!(t.run(run).stop_requested_reason.as_deref(), Some("manual"));
        t.gw.unauthorised
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let r = t.m.reconcile_pending_stop(run).await.unwrap();
        assert!(r.ok && r.stop_pending);
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    }

    #[tokio::test]
    async fn risk_that_cannot_be_acted_on_reaches_the_audit_trail_once_per_episode() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.store
            .execute_raw(&format!(
                "UPDATE sm_strategy_run SET mode = 'live' WHERE id = {}",
                run
            ))
            .unwrap();
        t.gw.unauthorised
            .store(true, std::sync::atomic::Ordering::SeqCst);
        t.m.process_tick(ATM_CE, "NFO", 121.0).await;
        t.m.process_tick(ATM_CE, "NFO", 122.0).await;
        let critical: Vec<Value> = t
            .events(sid)
            .into_iter()
            .filter(|e| e["kind"] == "leg_exit_rejected" && e["severity"] == "critical")
            .collect();
        assert_eq!(critical.len(), 1);
        assert_eq!(t.gw.placed().len(), 1, "nothing was pretended");
        // The session returning is recorded too.
        t.gw.unauthorised
            .store(false, std::sync::atomic::Ordering::SeqCst);
        t.m.process_tick(ATM_CE, "NFO", 123.0).await;
        assert!(t
            .event_kinds(sid)
            .contains(&"recovery_succeeded".to_string()));
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    }

    #[tokio::test]
    async fn a_daily_loss_limit_stops_the_run_on_the_session_total() {
        let t = t();
        let sid = t.make(config(
            "DLL",
            json!([short_call_leg()]),
            json!({"daily_loss_limit_inr": 1000}),
        ));
        let run = t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 110.0).await; // -650: inside
        assert!(t.run(run).stop_requested_reason.is_none());
        t.m.process_tick(ATM_CE, "NFO", 116.0).await; // -1040
        assert_eq!(
            t.run(run).stop_requested_reason.as_deref(),
            Some("daily_loss_limit")
        );
    }

    #[tokio::test]
    async fn trail_to_entry_fires_on_a_stop_driven_exit() {
        let t = t();
        let mut b = short_call_leg();
        b["id"] = json!(2);
        b["atm_offset"] = json!("OTM2");
        b["sl_pts"] = json!(50);
        let sid = t.make(config(
            "TTE",
            json!([short_call_leg(), b]),
            json!({"trail_sl_to_entry": true}),
        ));
        let run = t.start_filled(sid, 100.0).await;
        // Leg 2 in profit first, then leg 1 stops out.
        t.m.process_tick("NIFTY13OCT2624600CE", "NFO", 90.0).await;
        t.m.process_tick(ATM_CE, "NFO", 121.0).await;
        let s = t.m.state.snapshot(run).unwrap();
        assert!(s.trail_to_entry_active);
        assert_eq!(s.leg(2).unwrap().effective_sl, Some(100.0));
    }

    #[tokio::test]
    async fn a_stepped_trail_ratchets_and_then_fires() {
        let t = t();
        let mut l = short_call_leg();
        l["position"] = json!("B");
        l["trail"] = json!({"x": 10, "y": 5});
        let sid = t.make(config("Trail", json!([l]), json!({})));
        let run = t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 125.0).await; // two steps: 80 + 10
        assert_eq!(t.leg(run, 1).effective_sl, Some(90.0));
        t.m.process_tick(ATM_CE, "NFO", 115.0).await; // ratchet holds
        assert_eq!(t.leg(run, 1).effective_sl, Some(90.0));
        t.m.process_tick(ATM_CE, "NFO", 89.0).await;
        assert_eq!(t.gw.actions(), vec!["BUY", "SELL"]);
    }
}
// ===================================================================== signals

/// web: test/test_strategy_module_signals.py
mod strategy_module_signals {
    use super::*;
    use openalgo_desktop_lib::strategy::store::StrategyRow;

    fn strategy(t: &T, legs: Value, overrides: Value) -> StrategyRow {
        let sid = t.make(signal_config(legs, overrides));
        t.m.store.get_strategy(sid, USER).unwrap().unwrap()
    }

    async fn signal(
        t: &T,
        s: &StrategyRow,
        action: &str,
        leg: i64,
    ) -> openalgo_desktop_lib::strategy::signals::SignalResult {
        let fresh = t.m.store.get_strategy(s.id, USER).unwrap().unwrap();
        t.m.handle_signal(&fresh, action, Some(&json!(leg)), None, None)
            .await
    }

    fn current_run(t: &T, s: &StrategyRow) -> i64 {
        t.m.store
            .get_strategy(s.id, USER)
            .unwrap()
            .unwrap()
            .current_run_id
            .unwrap()
    }

    #[tokio::test]
    async fn a_leg_is_held_on_the_side_the_signal_opened_not_the_one_configured() {
        // PORTED DEFECT. A signal leg's configuration says which signals it
        // accepts, not which way it is held; the original never recorded a
        // side, so the risk core evaluated every signal leg as a short.
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        let r = signal(&t, &s, "short_entry", 1).await;
        assert!(r.acted(), "{:?}", r);
        let run = current_run(&t, &s);
        assert_eq!(t.leg(run, 1).position, "S");
        assert_eq!(t.gw.actions(), vec!["SELL"]);
    }

    #[tokio::test]
    async fn a_long_signal_opens_a_long() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        assert_eq!(t.leg(run, 1).position, "B");
        assert_eq!(t.gw.actions(), vec!["BUY"]);
    }

    #[tokio::test]
    async fn a_signal_entry_persists_the_live_position_reference() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        let entry = &t.orders(run)[0];
        assert_eq!(entry.position_ref.as_ref().map(|r| r.len()), Some(32));
        assert_eq!(t.leg(run, 1).position_ref, entry.position_ref);
    }

    #[tokio::test]
    async fn an_exit_covers_the_side_actually_held() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "short_entry", 1).await;
        let run = current_run(&t, &s);
        t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
            .await;
        let r = signal(&t, &s, "short_exit", 1).await;
        assert!(r.acted());
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    }

    #[tokio::test]
    async fn a_repeated_entry_is_a_noop_not_a_failure() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
            .await;
        let r = signal(&t, &s, "long_entry", 1).await;
        assert!(r.ok);
        assert_eq!(r.note.as_deref(), Some("already_long"));
        assert_eq!(t.gw.placed().len(), 1);
    }

    #[tokio::test]
    async fn an_exit_for_a_position_not_held_is_a_noop() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        let r = signal(&t, &s, "long_exit", 1).await;
        assert!(r.ok);
        assert_eq!(r.note.as_deref(), Some("no_matching_position"));
        assert!(t.gw.placed().is_empty());
    }

    #[tokio::test]
    async fn a_repeated_exit_alert_does_not_reverse_the_position() {
        // PORTED DEFECT 8. A leg stays open until its exit fill arrives, so
        // the second alert found the position still held and sent a second
        // closing order.
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
            .await;
        assert!(signal(&t, &s, "long_exit", 1).await.acted());
        let again = signal(&t, &s, "long_exit", 1).await;
        assert_eq!(again.note.as_deref(), Some("no_matching_position"));
        assert_eq!(t.gw.actions(), vec!["BUY", "SELL"]);
    }

    #[tokio::test]
    async fn a_direction_refuses_the_other_side() {
        let t = t();
        let s = strategy(
            &t,
            json!([signal_leg(1, "RELIANCE", "long")]),
            json!({"direction": "long_only"}),
        );
        let r = signal(&t, &s, "short_entry", 1).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("long_only"));
    }

    #[tokio::test]
    async fn a_leg_side_refuses_signals_it_does_not_accept() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "long")]), json!({}));
        let r = signal(&t, &s, "short_entry", 1).await;
        assert_eq!(r.error.as_deref(), Some("Leg 1 only accepts long signals"));
    }

    #[tokio::test]
    async fn an_unknown_leg_is_refused() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        let r = signal(&t, &s, "long_entry", 9).await;
        assert_eq!(r.error.as_deref(), Some("No leg matches this signal"));
    }

    #[tokio::test]
    async fn a_leg_can_be_found_by_symbol() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        let r =
            t.m.handle_signal(&s, "long_entry", None, Some("reliance"), Some("NSE"))
                .await;
        assert!(r.acted());
    }

    #[tokio::test]
    async fn signals_outside_the_trading_window_are_noops() {
        let t = t(); // 10:00 IST
        let s = strategy(
            &t,
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({"strategy_type": "intraday", "entry_time": "10:30", "exit_time": "15:00"}),
        );
        let r = signal(&t, &s, "long_entry", 1).await;
        assert_eq!(r.note.as_deref(), Some("outside_entry_window"));
        t.clock.set(ist(2026, 10, 7, 15, 5));
        let r = signal(&t, &s, "long_exit", 1).await;
        assert_eq!(r.note.as_deref(), Some("outside_trading_window"));
        assert!(t.gw.placed().is_empty());
    }

    #[tokio::test]
    async fn an_opposite_entry_squares_first_then_opens() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
            .await;
        let r = signal(&t, &s, "short_entry", 1).await;
        assert!(r.ok && r.flipped, "{:?}", r);
        assert_eq!(t.gw.actions(), vec!["BUY", "SELL", "SELL"]);
        let leg = t.leg(run, 1);
        assert_eq!(leg.position, "S");
        assert_eq!(leg.superseded.as_ref().unwrap().position, "B");
    }

    #[tokio::test]
    async fn an_uncarryable_short_is_refused_before_anything_is_squared() {
        let t = t();
        let s = strategy(
            &t,
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({"product": "CNC"}),
        );
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
            .await;
        let r = signal(&t, &s, "short_entry", 1).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("cannot be held short overnight"));
        assert_eq!(t.gw.actions(), vec!["BUY"], "the long was not liquidated");
    }

    #[tokio::test]
    async fn a_misspelt_contract_is_refused() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELAINCE", "both")]), json!({}));
        let r = signal(&t, &s, "long_entry", 1).await;
        assert!(r
            .error
            .unwrap()
            .contains("RELAINCE is not a contract on NSE"));
        assert!(t.gw.placed().is_empty());
    }

    #[tokio::test]
    async fn lots_mode_multiplies_by_the_master_lot_size() {
        let t = t();
        let leg = json!({"id": 1, "symbol": "RELIANCE27OCT26FUT", "exchange": "NFO", "side": "both",
                         "qty": 2, "qty_mode": "lots", "segment": "futures", "expiry": "current"});
        let s = strategy(&t, json!([leg]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        assert_eq!(t.gw.placed()[0].quantity, 1000);
    }

    #[tokio::test]
    async fn a_signal_exit_on_a_leg_does_not_end_the_session_run() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let run = current_run(&t, &s);
        t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
            .await;
        signal(&t, &s, "long_exit", 1).await;
        t.fill_last_exit(run, 1010.0).await;
        assert!(t.run(run).stopped_at.is_none());
        let leg = t.leg(run, 1);
        assert_eq!(leg.status, "closed");
        assert!((leg.realized_pnl - 100.0).abs() < 1e-9);
        // The same session can re-enter; realized accumulates on the leg.
        signal(&t, &s, "long_entry", 1).await;
        assert_eq!(current_run(&t, &s), run);
        assert!((t.leg(run, 1).realized_pnl - 100.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_stale_run_from_an_earlier_session_is_rolled_on_the_next_signal() {
        let t = t();
        let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
        signal(&t, &s, "long_entry", 1).await;
        let first = current_run(&t, &s);
        let id = t.orders(first)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "rejected", 0, 0.0).await;
        t.clock.set(ist(2026, 10, 8, 10, 0));
        signal(&t, &s, "long_entry", 1).await;
        let second = current_run(&t, &s);
        assert_ne!(first, second);
        assert_eq!(t.run(first).stop_reason.as_deref(), Some("eod"));
    }
}

// ===================================================================== order events

/// web: test/test_strategy_module_order_events.py
mod strategy_module_order_events {
    use super::*;

    #[tokio::test]
    async fn a_fill_is_applied_exactly_once() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "complete", 65, 100.0).await;
        t.frame(&id, "complete", 65, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        let exit = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind != "entry")
            .unwrap();
        let eid = exit.broker_order_id.unwrap();
        t.frame(&eid, "complete", 65, 90.0).await;
        t.frame(&eid, "complete", 65, 90.0).await;
        assert!((t.run(run).pnl_realized - 650.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn a_rejection_is_final() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "rejected", 0, 0.0).await;
        t.frame(&id, "open", 0, 0.0).await;
        assert_eq!(t.orders(run)[0].status, "rejected");
        assert_eq!(t.leg(run, 1).status, "rejected");
    }

    #[tokio::test]
    async fn a_zero_price_is_not_a_fill_price() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "complete", 65, 0.0).await;
        let leg = t.leg(run, 1);
        assert_eq!(leg.entry_avg, 0.0);
        assert_eq!(leg.status, "open", "the quantity is still managed");
        assert!(t.event_kinds(sid).contains(&"leg_entry_placed".to_string()));
    }

    #[tokio::test]
    async fn the_leg_is_resized_to_what_actually_filled() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "cancelled", 30, 100.0).await;
        assert_eq!(t.leg(run, 1).qty, 30);
        t.m.stop_run(run, USER, "manual").await;
        assert_eq!(t.gw.placed().last().unwrap().quantity, 30);
    }

    #[tokio::test]
    async fn a_partial_fill_then_the_rest_prices_only_the_delta() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "complete", 65, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        let eid = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind != "entry")
            .unwrap()
            .broker_order_id
            .unwrap();
        t.frame(&eid, "open", 25, 90.0).await;
        assert_eq!(t.leg(run, 1).qty, 40);
        // Cumulative average 94 over 65 means the last 40 traded at 96.5.
        t.frame(&eid, "complete", 65, 94.0).await;
        // realized = (100-90)*25 + (100-96.5)*40 = 250 + 140 = 390
        assert!(
            (t.run(run).pnl_realized - 390.0).abs() < 1e-6,
            "{}",
            t.run(run).pnl_realized
        );
    }

    #[tokio::test]
    async fn a_rejected_exit_on_the_stream_releases_its_claim() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.close_leg(run, 1, USER).await;
        let eid = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind != "entry")
            .unwrap()
            .broker_order_id
            .unwrap();
        t.frame(&eid, "rejected", 0, 0.0).await;
        let leg = t.leg(run, 1);
        assert!(leg.exit_kind.is_none() && leg.exit_order_id.is_none());
        assert!(t.m.close_leg(run, 1, USER).await.ok);
    }

    #[tokio::test]
    async fn a_fill_that_beats_its_row_is_held_and_replayed() {
        let t = t();
        let sid = t.default_strategy();
        // The update arrives for an id no row carries yet.
        t.frame("SB-1", "complete", 65, 100.0).await;
        assert_eq!(t.m.order_events.len(), 1);
        let run = t.start(sid).await.run_id.unwrap();
        assert_eq!(t.leg(run, 1).entry_status, "complete");
        assert_eq!(t.leg(run, 1).entry_avg, 100.0);
        assert!(t.m.order_events.is_empty());
    }

    #[tokio::test]
    async fn the_hold_buffer_is_bounded() {
        let t = t();
        for i in 0..2000 {
            t.frame(&format!("OTHER-{}", i), "complete", 1, 1.0).await;
        }
        assert!(t.m.order_events.len() <= 512);
    }

    #[tokio::test]
    async fn a_signal_flips_exit_fill_never_closes_the_new_position() {
        // The defect: a flip squares the long and opens the short at once;
        // applying the long's exit fill by leg alone closed the short, which
        // then vanished from every stop and square-off.
        let t = t();
        let sid = t.make(signal_config(
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({}),
        ));
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        t.m.handle_signal(&s, "long_entry", Some(&json!(1)), None, None)
            .await;
        let run =
            t.m.store
                .get_strategy(sid, USER)
                .unwrap()
                .unwrap()
                .current_run_id
                .unwrap();
        let entry = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&entry, "complete", 10, 1000.0).await;
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        assert!(
            t.m.handle_signal(&s, "short_entry", Some(&json!(1)), None, None)
                .await
                .flipped
        );
        let rows = t.orders(run);
        let long_exit = rows.iter().find(|o| o.kind == "exit_signal").unwrap();
        let short_entry = rows.iter().rev().find(|o| o.kind == "entry").unwrap();
        // The new short fills, then the OLD long's exit fills.
        t.frame(
            short_entry.broker_order_id.as_deref().unwrap(),
            "complete",
            10,
            990.0,
        )
        .await;
        t.frame(
            long_exit.broker_order_id.as_deref().unwrap(),
            "complete",
            10,
            990.0,
        )
        .await;
        let leg = t.leg(run, 1);
        assert_eq!(leg.position, "S");
        assert_eq!(leg.status, "open", "the new short is still managed");
        assert_eq!(leg.qty, 10);
        assert!(leg.superseded.is_none(), "the outgoing long settled");
        assert!((leg.realized_pnl + 100.0).abs() < 1e-9);
        // And it is still exitable by its stop.
        t.m.process_tick("RELIANCE", "NSE", 1011.0).await;
        assert_eq!(t.gw.actions(), vec!["BUY", "SELL", "SELL", "BUY"]);
    }

    #[tokio::test]
    async fn a_flips_refused_outgoing_exit_leaves_the_old_side_closable() {
        let t = t();
        let sid = t.make(signal_config(
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({}),
        ));
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        t.m.handle_signal(&s, "long_entry", Some(&json!(1)), None, None)
            .await;
        let run =
            t.m.store
                .get_strategy(sid, USER)
                .unwrap()
                .unwrap()
                .current_run_id
                .unwrap();
        let entry = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&entry, "complete", 10, 1000.0).await;
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        t.m.handle_signal(&s, "short_entry", Some(&json!(1)), None, None)
            .await;
        let long_exit = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind == "exit_signal")
            .unwrap();
        t.frame(
            long_exit.broker_order_id.as_deref().unwrap(),
            "rejected",
            0,
            0.0,
        )
        .await;
        let sup = t.leg(run, 1).superseded.unwrap();
        assert!(sup.exit_order_id.is_none() && sup.exit_claim_token.is_none());
        assert!(t
            .event_kinds(sid)
            .contains(&"flip_outgoing_exit_rejected".to_string()));
        // A long_exit now closes the outgoing long rather than reading flat.
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        let r =
            t.m.handle_signal(&s, "long_exit", Some(&json!(1)), None, None)
                .await;
        assert!(r.acted(), "{:?}", r);
        assert_eq!(t.gw.actions().last().unwrap(), "SELL");
    }
}
// ===================================================================== webhook

/// web: test/test_strategy_module_webhook.py
mod strategy_module_webhook {
    use super::*;
    use openalgo_desktop_lib::strategy::webhook::{ip_allowed, redact, COOLING_OFF};

    async fn hook(
        t: &T,
        token: &str,
        body: Value,
    ) -> openalgo_desktop_lib::strategy::webhook::WebhookOutcome {
        t.m.handle_webhook(
            token,
            body.to_string().as_bytes(),
            Some("10.0.0.1"),
            Some("TradingView"),
        )
        .await
    }

    #[tokio::test]
    async fn an_unknown_token_and_a_malformed_one_answer_identically() {
        let t = t();
        let a = t.m.handle_webhook("not-a-token", b"{}", None, None).await;
        let b =
            t.m.handle_webhook(&format!("oaws_{}", "A".repeat(43)), b"{}", None, None)
                .await;
        assert_eq!((a.status, a.body()), (b.status, b.body()));
        assert_eq!(a.status, 404);
        assert_eq!(a.result, "rejected_token");
        assert_eq!(t.m.store.count_unattributed_webhook_events().unwrap(), 2);
    }

    #[tokio::test]
    async fn the_kill_switch_outranks_everything_a_caller_controls() {
        let t = t();
        let (sid, token) = t.make_with_token(config("K", json!([short_call_leg()]), json!({})));
        t.m.store.set_webhook_locked(sid, USER, true).unwrap();
        let o = t.m.handle_webhook(&token, b"not json", None, None).await;
        assert_eq!((o.status, o.result.as_str()), (403, "rejected_locked"));
    }

    #[tokio::test]
    async fn an_address_outside_the_allowlist_is_refused_before_parsing() {
        let t = t();
        let (_, token) = t.make_with_token(config(
            "IP",
            json!([short_call_leg()]),
            json!({"webhook_ip_allowlist": ["192.168.1.0/24"]}),
        ));
        let o =
            t.m.handle_webhook(&token, b"garbage", Some("10.0.0.1"), None)
                .await;
        assert_eq!((o.status, o.result.as_str()), (403, "rejected_ip"));
        assert!(ip_allowed(Some("192.168.1.77"), &json!(["192.168.1.0/24"])));
        assert!(ip_allowed(
            Some("::ffff:192.168.1.5"),
            &json!(["192.168.1.0/24"])
        ));
        assert!(!ip_allowed(None, &json!(["192.168.1.0/24"])));
        assert!(ip_allowed(Some("1.2.3.4"), &json!([])));
        assert!(ip_allowed(
            Some("1.2.3.4"),
            &json!(["bad entry", "1.2.3.4"])
        ));
    }

    #[tokio::test]
    async fn payload_errors_are_refused_with_400() {
        let t = t();
        let (_, token) = t.make_with_token(config("P", json!([short_call_leg()]), json!({})));
        for (body, msg) in [
            ("", "The request body is empty"),
            ("not json", "The request body is not valid JSON"),
            ("[1,2]", "The request body must be a JSON object"),
        ] {
            let o =
                t.m.handle_webhook(&token, body.as_bytes(), None, None)
                    .await;
            assert_eq!(
                (o.status, o.result.as_str(), o.message.as_str()),
                (400, "rejected_payload", msg)
            );
        }
        let big = format!("{{\"action\":\"start\",\"x\":\"{}\"}}", "a".repeat(20000));
        let o = t.m.handle_webhook(&token, big.as_bytes(), None, None).await;
        assert_eq!(o.result, "rejected_payload");
    }

    #[tokio::test]
    async fn each_kind_refuses_the_others_vocabulary() {
        let t = t();
        let (_, batch) = t.make_with_token(config("B", json!([short_call_leg()]), json!({})));
        let o = hook(&t, &batch, json!({"action": "long_entry"})).await;
        assert_eq!(
            (o.status, o.result.as_str()),
            (400, "rejected_invalid_action")
        );
        assert_eq!(o.message, "'action' must be one of start, stop");
        let (_, sig) = t.make_with_token(signal_config(
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({}),
        ));
        let o = hook(&t, &sig, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!(
            o.message,
            "'action' must be one of long_entry, long_exit, short_entry, short_exit"
        );
    }

    #[tokio::test]
    async fn start_requires_a_mode_and_live_requires_the_opt_in() {
        let t = t();
        let (_, token) = t.make_with_token(config("M", json!([short_call_leg()]), json!({})));
        let o = hook(&t, &token, json!({"action": "start"})).await;
        assert_eq!(
            (o.status, o.result.as_str()),
            (400, "rejected_invalid_action")
        );
        let o = hook(&t, &token, json!({"action": "start", "mode": "paper"})).await;
        assert_eq!(o.result, "rejected_invalid_action");
        let o = hook(&t, &token, json!({"action": "start", "mode": "live"})).await;
        assert_eq!(
            (o.status, o.result.as_str()),
            (403, "rejected_live_disabled")
        );
        assert!(t.gw.placed().is_empty());
    }

    #[tokio::test]
    async fn a_start_is_accepted_and_names_its_run() {
        let t = t();
        let (sid, token) = t.make_with_token(config("S", json!([short_call_leg()]), json!({})));
        let o = hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!((o.status, o.result.as_str()), (200, "ok"), "{:?}", o);
        let run = o.run_id.unwrap();
        assert_eq!(t.run(run).trigger_source, "webhook");
        assert_eq!(t.run(run).webhook_event_id, o.webhook_event_id);
        let audit = t.m.store.list_webhook_events(sid, 10).unwrap();
        assert_eq!(audit[0]["result"], "ok");
        assert_eq!(audit[0]["user_agent"], "TradingView");
    }

    #[tokio::test]
    async fn a_retry_inside_the_window_is_deduplicated_as_success() {
        let t = t();
        let (_, token) = t.make_with_token(config("D", json!([short_call_leg()]), json!({})));
        hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        let o = hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!(
            (o.status, o.result.as_str(), o.ok),
            (200, "rejected_dedupe", true)
        );
        assert_eq!(t.gw.placed().len(), 1);
    }

    #[tokio::test]
    async fn a_stopped_strategy_cools_off_before_a_new_start() {
        let t = t();
        let (sid, token) = t.make_with_token(config("C", json!([short_call_leg()]), json!({})));
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 100.0).await;
        let o = hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!((o.status, o.result.as_str()), (409, "rejected_cooling_off"));
        t.m.webhook.advance(COOLING_OFF);
        let o = hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!(o.result, "ok");
    }

    #[tokio::test]
    async fn a_stop_for_an_already_flat_strategy_is_a_success() {
        let t = t();
        let (_, token) = t.make_with_token(config("F", json!([short_call_leg()]), json!({})));
        let o = hook(&t, &token, json!({"action": "stop"})).await;
        assert_eq!((o.status, o.result.as_str()), (200, "ok"));
    }

    #[tokio::test]
    async fn an_engine_refusal_releases_the_dedupe_claim() {
        let t = t();
        let (_, token) = t.make_with_token(config("E", json!([short_call_leg()]), json!({})));
        t.gw.reject_next("Insufficient funds");
        let o = hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!(
            (o.status, o.result.as_str()),
            (500, "rejected_engine_error")
        );
        t.m.webhook.advance(COOLING_OFF);
        let o = hook(&t, &token, json!({"action": "start", "mode": "sandbox"})).await;
        assert_eq!(o.result, "ok", "the retry is not swallowed as a duplicate");
    }

    #[tokio::test]
    async fn signal_actions_skip_the_dedupe_window() {
        let t = t();
        let (_, token) = t.make_with_token(signal_config(
            json!([signal_leg(1, "RELIANCE", "both")]),
            json!({}),
        ));
        let a = hook(&t, &token, json!({"action": "long_entry", "leg_id": 1})).await;
        assert_eq!(a.message, "Signal accepted");
        let b = hook(&t, &token, json!({"action": "long_entry", "leg_id": 1})).await;
        assert_eq!(b.result, "ok");
        // An exit needs a confirmed quantity: fill the entry first.
        let run = a.run_id.unwrap();
        let entry = t
            .orders(run)
            .into_iter()
            .find(|o| o.kind == "entry")
            .unwrap();
        t.frame(
            entry.broker_order_id.as_deref().unwrap(),
            "complete",
            entry.qty,
            1000.0,
        )
        .await;
        let c = hook(&t, &token, json!({"action": "long_exit", "leg_id": 1})).await;
        assert_eq!(c.result, "ok");
        let d = hook(&t, &token, json!({"action": "short_entry", "leg_id": 9})).await;
        assert_eq!(
            (d.status, d.result.as_str()),
            (400, "rejected_invalid_action")
        );
    }

    #[tokio::test]
    async fn the_token_never_reaches_the_audit_row() {
        let t = t();
        let (sid, token) = t.make_with_token(config("R", json!([short_call_leg()]), json!({})));
        let body = json!({"action": "nope", "url": format!("https://x/strategy/webhook/{}", token),
                          "api_key": "secret", "nested": {"note": "oaws_something"}});
        hook(&t, &token, body).await;
        let row = &t.m.store.list_webhook_events(sid, 1).unwrap()[0];
        let text = row["payload"].to_string();
        assert!(!text.contains(&token));
        assert_eq!(row["payload"]["url"], "[redacted]");
        assert_eq!(row["payload"]["api_key"], "[redacted]");
        assert_eq!(row["payload"]["nested"]["note"], "[redacted]");
        let deep = redact(&json!({"a": {"b": {"c": {"d": {"e": {"f": 1}}}}}}), "", 0);
        assert!(deep.to_string().contains("[truncated]"));
    }

    #[tokio::test]
    async fn unattributed_audit_rows_are_capped() {
        let t = t();
        for _ in 0..1010 {
            t.m.handle_webhook("x", b"{}", None, None).await;
        }
        assert_eq!(t.m.store.count_unattributed_webhook_events().unwrap(), 1000);
    }

    #[tokio::test]
    async fn failures_per_webhook_are_bounded_and_cleared_on_unlock() {
        use openalgo_desktop_lib::strategy::webhook::LOCKOUT_FAILURES;
        let t = t();
        for sid in 0..5000 {
            assert!(!t.m.webhook.record_webhook_failure(sid));
        }
        assert!(t.m.webhook.tracked() <= 4096);
        for _ in 0..LOCKOUT_FAILURES - 1 {
            assert!(!t.m.webhook.record_webhook_failure(-1));
        }
        t.m.webhook.clear_webhook_failures(-1);
        assert!(!t.m.webhook.record_webhook_failure(-1), "history cleared");
    }
}

// ===================================================================== scheduler

/// web: test/test_strategy_module_scheduler.py
mod strategy_module_scheduler {
    use super::*;
    use openalgo_desktop_lib::strategy::scheduler::{
        planned_jobs, start_job_id, stop_job_id, JobFunc, MISFIRE_GRACE, TIMEZONE,
    };

    fn scheduler(enabled: bool, start: Option<&str>, stop: Option<&str>) -> Value {
        json!({"enabled": enabled, "days": ["MON", "TUE", "WED", "THU", "FRI"],
               "start_time": start, "auto_stop_time": stop, "default_mode": "sandbox"})
    }

    fn make(t: &T, overrides: Value) -> i64 {
        t.make(config("Sched", json!([short_call_leg()]), overrides))
    }

    #[test]
    fn the_timezone_reaches_every_trigger() {
        // PORTED DEFECT. Flow's and Historify's cron jobs carry no timezone,
        // so a 09:20 IST entry fires at server-local 09:20.
        let t = t();
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
        );
        t.m.sync_strategy_jobs(sid);
        for id in [start_job_id(sid), stop_job_id(sid)] {
            assert_eq!(t.m.scheduler.get(&id).unwrap().timezone, TIMEZONE);
        }
        assert_eq!(TIMEZONE, "Asia/Kolkata");
    }

    #[test]
    fn every_job_carries_the_project_job_defaults() {
        // PORTED DEFECT. python_strategy inherits a 1 s misfire grace, so a
        // 09:15 entry that slips two seconds is dropped silently.
        let t = t();
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
        );
        t.m.sync_strategy_jobs(sid);
        for j in t.m.scheduler.jobs() {
            assert!(j.coalesce);
            assert_eq!(j.max_instances, 1);
            assert_eq!(j.misfire_grace, MISFIRE_GRACE);
            assert_eq!(MISFIRE_GRACE.as_secs(), 60);
        }
    }

    #[test]
    fn jobs_are_plain_values_with_plain_arguments() {
        // PORTED DEFECT. python_strategy scheduled closures no store can hold.
        let t = t();
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
        );
        t.m.sync_strategy_jobs(sid);
        let start = t.m.scheduler.get(&start_job_id(sid)).unwrap();
        let stop = t.m.scheduler.get(&stop_job_id(sid)).unwrap();
        assert_eq!(
            (start.func, start.strategy_id),
            (JobFunc::RunScheduledStart, sid)
        );
        assert_eq!(
            (stop.func, stop.strategy_id),
            (JobFunc::RunScheduledStop, sid)
        );
        assert_eq!(
            (start.hour, start.minute, stop.hour, stop.minute),
            (9, 20, 15, 10)
        );
    }

    #[test]
    fn an_invalid_time_is_skipped_rather_than_installed() {
        let t = t();
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("9:70"), Some("not a time"))}),
        );
        assert!(t.m.sync_strategy_jobs(sid).is_empty());
    }

    #[test]
    fn a_broken_stop_time_does_not_take_the_start_job_down_with_it() {
        let t = t();
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("25:00"))}),
        );
        assert_eq!(t.m.sync_strategy_jobs(sid), vec![start_job_id(sid)]);
    }

    #[test]
    fn an_intraday_exit_time_installs_the_square_off_the_original_never_scheduled() {
        // PORTED DEFECT, and the one real gap closed: with exit_time set and
        // auto_stop_time blank, the original installed no stop job at all.
        let t = t();
        let sid = make(
            &t,
            json!({"strategy_type": "intraday", "entry_time": "09:20",
            "exit_time": "15:20", "scheduler": scheduler(true, Some("09:20"), None)}),
        );
        let installed = t.m.sync_strategy_jobs(sid);
        assert!(installed.contains(&stop_job_id(sid)));
        let j = t.m.scheduler.get(&stop_job_id(sid)).unwrap();
        assert_eq!((j.hour, j.minute), (15, 20));
    }

    #[test]
    fn an_intraday_exit_time_is_squared_off_with_the_scheduler_switched_off() {
        let t = t();
        let sid = make(
            &t,
            json!({"strategy_type": "intraday", "entry_time": "09:20",
            "exit_time": "15:20", "scheduler": scheduler(false, None, None)}),
        );
        assert_eq!(t.m.sync_strategy_jobs(sid), vec![stop_job_id(sid)]);
        let j = t.m.scheduler.get(&stop_job_id(sid)).unwrap();
        assert_eq!(j.days.len(), 5, "weekdays");
    }

    #[test]
    fn an_unknown_day_rejects_the_whole_list() {
        let t = t();
        let row =
            t.m.store
                .get_strategy_unscoped(make(
                    &t,
                    json!({
            "scheduler": {"enabled": true, "days": ["MON", "FUNDAY"], "start_time": "09:20",
                          "auto_stop_time": "15:10"}}),
                ))
                .unwrap()
                .unwrap();
        assert!(planned_jobs(&row).is_empty());
    }

    #[test]
    fn deleting_a_strategy_drops_its_jobs_and_orphans_are_swept() {
        let t = t();
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
        );
        t.m.sync_all_jobs();
        assert_eq!(t.m.scheduler.len(), 2);
        t.m.store.delete_strategy(sid, USER).unwrap();
        let r = t.m.sync_all_jobs();
        assert_eq!(r["orphans_removed"], 2);
        assert!(t.m.scheduler.is_empty());
    }

    #[tokio::test]
    async fn a_job_fires_once_in_its_ist_slot_within_the_grace() {
        let t = t_at(ist(2026, 10, 7, 9, 19));
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
        );
        t.m.sync_strategy_jobs(sid);
        assert!(t.m.run_due_jobs().await.is_empty());
        t.clock
            .set(ist(2026, 10, 7, 9, 20) + chrono::Duration::seconds(30));
        assert_eq!(t.m.run_due_jobs().await, vec![start_job_id(sid)]);
        assert!(
            t.m.run_due_jobs().await.is_empty(),
            "coalesced: once per slot"
        );
        assert_eq!(
            t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
            "running"
        );
        assert_eq!(
            t.run(
                t.m.store.list_runs(sid, 1).unwrap()[0]["id"]
                    .as_i64()
                    .unwrap()
            )
            .trigger_source,
            "scheduler"
        );
    }

    #[tokio::test]
    async fn a_slot_missed_by_more_than_the_grace_is_dropped() {
        let t = t_at(ist(2026, 10, 7, 9, 22));
        let sid = make(
            &t,
            json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
        );
        t.m.sync_strategy_jobs(sid);
        assert!(t.m.run_due_jobs().await.is_empty());
    }

    #[tokio::test]
    async fn a_scheduled_live_start_without_the_opt_in_is_refused_and_recorded() {
        let t = t();
        let mut sch = scheduler(true, Some("09:20"), Some("15:10"));
        sch["default_mode"] = json!("live");
        let sid = make(&t, json!({"scheduler": sch}));
        t.m.run_scheduled_start(sid).await;
        assert!(t.gw.placed().is_empty());
        assert!(t.event_kinds(sid).contains(&"live_disabled".to_string()));
    }

    #[tokio::test]
    async fn the_scheduled_square_off_stops_a_running_strategy() {
        let t = t();
        let sid = make(&t, json!({}));
        let run = t.start_filled(sid, 100.0).await;
        t.m.run_scheduled_stop(sid).await;
        assert_eq!(
            t.run(run).stop_requested_reason.as_deref(),
            Some("scheduler")
        );
        // `exit_scheduler` is not an order kind, so the square-off records
        // `exit_close_all`, as on the web.
        assert_eq!(t.orders(run).last().unwrap().kind, "exit_close_all");
    }

    #[tokio::test]
    async fn pending_stops_are_retried_by_the_reconcile_pass() {
        let t = t();
        let sid = make(&t, json!({}));
        let run = t.start_filled(sid, 100.0).await;
        t.gw.reject_next("Rate limited");
        assert!(t.m.stop_run(run, USER, "manual").await.stop_pending);
        let r = t.m.reconcile_pending_stops().await;
        assert_eq!(r["examined"], 1);
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
    }
}
// ===================================================================== recovery

/// web: test/test_strategy_module_recovery.py
mod strategy_module_recovery {
    use super::*;
    use openalgo_desktop_lib::strategy::checkpoint::write_once;
    use openalgo_desktop_lib::strategy::recovery::{
        normalise_order_status, order_is_dead, order_is_filled, order_is_working, recover_all,
        recover_run,
    };

    #[test]
    fn a_working_broker_status_reads_the_same_way_everywhere() {
        // PORTED DEFECT. Two normalisers disagreed on exactly these: one read
        // them as live orders, the other as unknown and therefore dead.
        for s in [
            "submitted",
            "trigger_pending",
            "TRIGGER PENDING",
            "modified",
        ] {
            assert!(order_is_working(s), "{}", s);
            assert!(!order_is_filled(s) && !order_is_dead(s));
        }
    }

    #[test]
    fn an_unrecognised_status_is_read_as_working_rather_than_dead() {
        assert_eq!(normalise_order_status("some-new-broker-word"), "open");
        assert!(!order_is_dead("some-new-broker-word"));
        assert_eq!(normalise_order_status("Filled"), "complete");
        assert_eq!(normalise_order_status("canceled"), "cancelled");
    }

    /// Simulate a restart: drop the in-memory state, keep the database.
    fn crash(t: &T, run: i64) {
        t.m.state.clear(run);
    }

    #[tokio::test]
    async fn a_held_run_is_rebuilt_from_its_orders_and_checkpoint() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "complete", 65, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 95.0).await;
        assert_eq!(write_once(&t.m, Some(false)), 1);
        crash(&t, run);
        let r = recover_run(&t.m, run).await;
        assert!(r.ok, "{:?}", r);
        assert_eq!(r.symbols, vec![(ATM_CE.to_string(), "NFO".to_string())]);
        let leg = t.leg(run, 1);
        assert_eq!(leg.position, "S", "the side comes from the entry action");
        assert_eq!(leg.entry_avg, 100.0);
        assert_eq!(leg.qty, 65);
        assert_eq!(leg.sl_pts, Some(20.0));
        assert_eq!(
            leg.lowest_price,
            Some(95.0),
            "volatile state from the checkpoint"
        );
        // And it is managed: its stop still fires.
        t.m.process_tick(ATM_CE, "NFO", 121.0).await;
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    }

    #[tokio::test]
    async fn a_run_that_died_flat_is_finished_at_startup() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        let id = t.orders(run)[0].broker_order_id.clone().unwrap();
        t.frame(&id, "rejected", 0, 0.0).await;
        crash(&t, run);
        let resumed = recover_all(&t.m).await.unwrap();
        assert!(resumed.is_empty());
        assert!(t.run(run).stopped_at.is_some());
        assert_eq!(
            t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
            "stopped"
        );
    }

    #[tokio::test]
    async fn a_dead_order_is_never_upgraded_by_a_checkpoint() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await; // checkpoint will say complete
        write_once(&t.m, Some(false));
        t.m.store
            .execute_raw(&format!(
                "UPDATE sm_strategy_order SET status = 'rejected', filled_qty = 0 WHERE run_id = {}",
                run
            ))
            .unwrap();
        crash(&t, run);
        let r = recover_run(&t.m, run).await;
        assert!(r.finalised, "{:?}", r);
    }

    #[tokio::test]
    async fn a_working_exit_without_a_confirmed_owner_stays_reserved() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start(sid).await.run_id.unwrap();
        // An exit row that is working while the entry never filled.
        t.m.store
            .execute_raw(&format!(
                "INSERT INTO sm_strategy_order (run_id, leg_id, kind, position_ref, broker_order_id, symbol, exchange, action, qty, pricetype, status, placed_at) \
                 SELECT run_id, leg_id, 'exit_sl', position_ref, 'X-1', symbol, exchange, 'BUY', qty, 'MARKET', 'open', placed_at FROM sm_strategy_order WHERE run_id = {}",
                run
            ))
            .unwrap();
        crash(&t, run);
        let r = recover_run(&t.m, run).await;
        assert!(!r.ok && !r.finalised, "{:?}", r);
        assert!(
            t.run(run).stopped_at.is_none(),
            "not finalised over possible exposure"
        );
        assert!(t.event_kinds(sid).contains(&"recovery_failed".to_string()));
    }

    #[tokio::test]
    async fn recovery_releases_an_empty_claim_when_a_process_dies_before_run_linkage() {
        let t = t();
        let sid = t.default_strategy();
        assert!(t.m.store.claim_strategy_for_run(sid).unwrap());
        let run =
            t.m.store
                .create_run(sid, "sandbox", "sandbox", "manual", None, None)
                .unwrap();
        let r = recover_run(&t.m, run).await;
        assert!(r.finalised);
        assert!(t.run(run).stopped_at.is_some());
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        assert_eq!((s.status.as_str(), s.current_run_id), ("stopped", None));
    }

    #[tokio::test]
    async fn recovery_is_idempotent_over_live_state() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        let before = t.m.state.snapshot(run).unwrap();
        assert!(recover_run(&t.m, run).await.ok);
        assert_eq!(t.m.state.snapshot(run).unwrap(), before);
    }

    #[tokio::test]
    async fn a_durable_pending_stop_is_recovered_with_its_reason() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.gw.reject_next("Rate limited");
        t.m.stop_run(run, USER, "overall_sl").await;
        crash(&t, run);
        assert!(recover_run(&t.m, run).await.ok);
        assert!(t.m.state.snapshot(run).unwrap().stopping);
        let r = t.m.reconcile_pending_stop(run).await.unwrap();
        assert!(r.ok, "{:?}", r);
        assert!(
            r.stop_pending && r.exits.iter().any(|e| e["ok"] == json!(true)),
            "the retry placed the exit: {:?}\n{:?}\n{:?}",
            r,
            t.m.state.snapshot(run),
            t.orders(run)
        );
        t.fill_last_exit(run, 100.0).await;
        assert_eq!(t.run(run).stop_reason.as_deref(), Some("overall_sl"));
    }
}

// ===================================================================== checkpoint, broadcast, db

/// web: test/test_strategy_module_db.py, test_strategy_module_broadcast.py
mod strategy_module_db {
    use super::*;
    use openalgo_desktop_lib::strategy::checkpoint::{write_once, CHECKPOINT_KEEP};
    use openalgo_desktop_lib::strategy::store::{hash_webhook_token, WEBHOOK_TOKEN_PREFIX};

    #[test]
    fn a_token_is_shown_once_and_stored_only_as_a_digest() {
        let t = t();
        let (sid, token) = t.make_with_token(config("Tok", json!([short_call_leg()]), json!({})));
        assert!(token.starts_with(WEBHOOK_TOKEN_PREFIX));
        let row = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        assert_eq!(row.webhook_token_hash, hash_webhook_token(&token));
        assert!(!row.to_dict(true).to_string().contains(&token));
        assert!(row.to_dict(true).get("webhook_token_hash").is_none());
        let rotated = t.m.store.rotate_webhook_token(sid, USER).unwrap();
        assert!(t
            .m
            .store
            .get_strategy_by_webhook_token(&token)
            .unwrap()
            .is_none());
        assert_eq!(
            t.m.store
                .get_strategy_by_webhook_token(&rotated)
                .unwrap()
                .unwrap()
                .id,
            sid
        );
    }

    #[test]
    fn a_duplicate_name_is_refused_for_the_same_user() {
        let t = t();
        t.make(config("Dup", json!([short_call_leg()]), json!({})));
        assert!(t
            .m
            .store
            .create_strategy(USER, &config("Dup", json!([]), json!({})))
            .is_err());
        assert!(t
            .m
            .store
            .create_strategy("someone", &config("Dup", json!([]), json!({})))
            .is_ok());
    }

    #[test]
    fn a_strategy_that_is_not_yours_reads_as_absent() {
        let t = t();
        let sid = t.default_strategy();
        assert!(t.m.store.get_strategy(sid, "intruder").unwrap().is_none());
    }

    #[tokio::test]
    async fn deleting_a_strategy_removes_every_child_row() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        write_once(&t.m, Some(false));
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 100.0).await;
        t.m.store.delete_strategy(sid, USER).unwrap();
        assert!(t.m.store.list_runs(sid, 10).unwrap().is_empty());
        assert!(t.m.store.list_orders(run).unwrap().is_empty());
        assert_eq!(t.m.store.count_checkpoints(run).unwrap(), 0);
        assert!(t.events(sid).is_empty());
    }

    #[test]
    fn a_running_strategy_cannot_be_edited_or_deleted() {
        let t = t();
        let sid = t.default_strategy();
        t.m.store.set_strategy_status(sid, "running", None).unwrap();
        assert!(t.m.store.delete_strategy(sid, USER).is_err());
        let mut ch = serde_json::Map::new();
        ch.insert("name".into(), json!("x"));
        assert!(t.m.store.update_strategy(sid, USER, &ch).is_err());
    }

    #[test]
    fn the_kind_cannot_change_by_update() {
        let t = t();
        let sid = t.default_strategy();
        let mut ch = serde_json::Map::new();
        ch.insert("strategy_kind".into(), json!("signal"));
        let e = t.m.store.update_strategy(sid, USER, &ch).unwrap_err();
        assert!(e
            .to_string()
            .contains("cannot change between batch and signal"));
    }

    #[tokio::test]
    async fn checkpoints_are_pruned_to_the_newest() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        for _ in 0..(CHECKPOINT_KEEP + 30) {
            write_once(&t.m, Some(false));
        }
        assert_eq!(
            t.m.store.count_checkpoints(run).unwrap(),
            CHECKPOINT_KEEP + 30
        );
        write_once(&t.m, Some(true));
        assert_eq!(t.m.store.count_checkpoints(run).unwrap(), CHECKPOINT_KEEP);
    }

    #[tokio::test]
    async fn the_list_carries_the_last_finalised_run() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 90.0).await;
        let list = t.m.store.list_strategies(USER, None, None).unwrap();
        assert_eq!(list[0]["last_finalized_run"]["id"], run);
        assert_eq!(list[0]["last_finalized_run"]["pnl_realized"], 650.0);
        assert!(list[0].get("legs").is_none());
        let filtered =
            t.m.store
                .list_strategies(USER, None, Some("engine"))
                .unwrap();
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn the_claim_is_one_conditional_update() {
        let t = t();
        let sid = t.default_strategy();
        assert!(t.m.store.claim_strategy_for_run(sid).unwrap());
        assert!(!t.m.store.claim_strategy_for_run(sid).unwrap());
    }

    #[test]
    fn timestamps_render_with_an_explicit_utc_offset() {
        let t = t();
        let sid = t.default_strategy();
        let d =
            t.m.store
                .get_strategy(sid, USER)
                .unwrap()
                .unwrap()
                .to_dict(true);
        assert!(d["created_at"].as_str().unwrap().ends_with("+00:00"));
    }
}

/// web: test/test_strategy_module_broadcast.py
mod strategy_module_broadcast {
    use super::*;
    use openalgo_desktop_lib::strategy::broadcast::room_for;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn frames_go_to_the_strategy_room_with_the_envelope() {
        let t = t();
        t.rooms.watching.store(true, Ordering::SeqCst);
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 90.0).await;
        let frames = t.rooms.frames.lock().clone();
        assert!(frames.iter().all(|f| f.0 == room_for(sid)));
        let kinds: std::collections::HashSet<String> = frames.iter().map(|f| f.1.clone()).collect();
        for k in [
            "strategy_event",
            "strategy_delta",
            "strategy_order_update",
            "strategy_run_update",
            "strategy_terminal",
        ] {
            assert!(kinds.contains(k), "missing {}", k);
        }
        let terminal = frames.iter().find(|f| f.1 == "strategy_terminal").unwrap();
        assert_eq!(terminal.2["type"], "terminal");
        assert_eq!(terminal.2["strategy_id"], sid);
        assert_eq!(terminal.2["run_id"], run);
        assert_eq!(terminal.2["stop_reason"], "manual");
        assert_eq!(terminal.2["pnl_realized"], 650.0);
        assert!(terminal.2["ts_ms"].as_i64().unwrap() > 0);
        assert!(terminal.2["ts"].as_str().unwrap().ends_with("+05:30"));
        assert_eq!(
            t.m.broadcast.tracked(),
            0,
            "terminal drops the throttle entry"
        );
    }

    #[tokio::test]
    async fn an_unwatched_run_sends_nothing() {
        let t = t();
        let sid = t.default_strategy();
        t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 101.0).await;
        assert!(t.rooms.frames.lock().is_empty());
    }

    #[tokio::test]
    async fn deltas_are_throttled_and_one_offs_are_not() {
        let t = t();
        t.rooms.watching.store(true, Ordering::SeqCst);
        let sid = t.default_strategy();
        t.start_filled(sid, 100.0).await;
        t.rooms.frames.lock().clear();
        for p in 0..20 {
            t.m.process_tick(ATM_CE, "NFO", 101.0 + p as f64 * 0.05)
                .await;
        }
        let deltas = t
            .rooms
            .events()
            .iter()
            .filter(|e| *e == "strategy_delta")
            .count();
        assert!(deltas >= 1 && deltas < 20, "{} deltas", deltas);
    }

    #[tokio::test]
    async fn the_snapshot_carries_every_leg_in_id_order() {
        let t = t();
        let mut b = short_call_leg();
        b["id"] = json!(2);
        let sid = t.make(config("Snap", json!([b, short_call_leg()]), json!({})));
        let run = t.start(sid).await.run_id.unwrap();
        let p =
            t.m.broadcast
                .snapshot_payload(&t.m.state.snapshot(run).unwrap());
        assert_eq!(p["type"], "snapshot");
        let ids: Vec<i64> = p["legs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["leg_id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2]);
        for k in [
            "mtm_realized",
            "mtm_unrealized",
            "mtm_total",
            "peak",
            "trough",
            "lock_armed",
            "lock_floor",
            "trail_to_entry_active",
            "tick_source_degraded",
        ] {
            assert!(p.get(k).is_some(), "{}", k);
        }
        for k in [
            "symbol",
            "position",
            "qty",
            "status",
            "entry_status",
            "ltp",
            "entry_avg",
            "mtm",
            "effective_sl",
            "favorable_points",
            "tick_source",
        ] {
            assert!(p["legs"][0].get(k).is_some(), "{}", k);
        }
    }
}
// ===================================================================== concurrency

/// Barrier-synchronised proofs of the order-path invariants (web
/// test/test_gthread_strategy_*.py).
mod concurrency {
    use super::*;
    use openalgo_desktop_lib::strategy::state::{
        new_leg_state, ClaimId, LegSpec, RunState, StateRegistry,
    };
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Barrier};

    #[test]
    fn two_rules_racing_on_one_leg_claim_it_exactly_once() {
        for _ in 0..200 {
            let reg = Arc::new(StateRegistry::new());
            let mut leg = new_leg_state(&LegSpec {
                leg_id: 1,
                position: "B".into(),
                symbol: "X".into(),
                exchange: "NFO".into(),
                quantity: 1,
                ..Default::default()
            })
            .unwrap();
            leg.status = "open".into();
            leg.entry_status = "complete".into();
            reg.install(RunState::new(1, 1, vec![leg]));
            let barrier = Arc::new(Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let (reg, barrier) = (reg.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        if i % 2 == 0 {
                            reg.claim_leg_exit(1, 1, "exit_sl").is_some() as usize
                        } else {
                            reg.claim_legs_for_exit(1, &[1], "exit_close_all").0.len()
                        }
                    })
                })
                .collect();
            let wins: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
            assert_eq!(wins, 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stop_loss_and_a_manual_close_send_exactly_one_exit() {
        for _ in 0..10 {
            let t = t();
            let sid = t.default_strategy();
            let run = t.start_filled(sid, 100.0).await;
            // Widen the window: the first dispatch is in flight for a while.
            t.gw.delay_ms.store(30, Ordering::SeqCst);
            let barrier = Arc::new(tokio::sync::Barrier::new(3));
            let (m1, m2, m3) = (t.m.clone(), t.m.clone(), t.m.clone());
            let (b1, b2, b3) = (barrier.clone(), barrier.clone(), barrier.clone());
            let a = tokio::spawn(async move {
                b1.wait().await;
                m1.process_tick(ATM_CE, "NFO", 125.0).await;
            });
            let b = tokio::spawn(async move {
                b2.wait().await;
                m2.close_leg(run, 1, USER).await;
            });
            let c = tokio::spawn(async move {
                b3.wait().await;
                m3.stop_run(run, USER, "manual").await;
            });
            let _ = tokio::join!(a, b, c);
            let exits: Vec<_> = t
                .orders(run)
                .into_iter()
                .filter(|o| o.kind != "entry")
                .collect();
            let sent: Vec<_> = exits.iter().filter(|o| o.status != "rejected").collect();
            assert_eq!(sent.len(), 1, "{:?}", exits);
            assert_eq!(t.gw.actions().iter().filter(|a| *a == "BUY").count(), 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_refused_dispatch_releases_the_claim() {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        t.gw.reject_next("Broker busy");
        t.m.process_tick(ATM_CE, "NFO", 121.0).await;
        let leg = t.leg(run, 1);
        assert!(
            leg.exit_kind.is_none()
                && leg.exit_claim_token.is_none()
                && leg.exit_order_id.is_none()
        );
        // The very next tick through the stop is not mistaken for a duplicate.
        t.m.process_tick(ATM_CE, "NFO", 122.0).await;
        assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
        // And a release by the wrong claim id is refused.
        assert!(!t.m.state.release_leg_exit(run, 1, &ClaimId::Row(-1)));
        assert!(t.leg(run, 1).exit_order_id.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_starts_place_one_set_of_entries() {
        let t = t();
        let sid = t.default_strategy();
        let barrier = Arc::new(tokio::sync::Barrier::new(4));
        let mut handles = vec![];
        for _ in 0..4 {
            let (m, b) = (t.m.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                b.wait().await;
                m.start_run(sid, USER, "sandbox", "manual", None).await.ok
            }));
        }
        let mut oks = 0;
        for h in handles {
            if h.await.unwrap() {
                oks += 1;
            }
        }
        assert_eq!(oks, 1);
        assert_eq!(t.gw.placed().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_alerts_on_one_bar_join_one_signal_run() {
        let t = t();
        let sid = t.make(signal_config(
            json!([
                signal_leg(1, "RELIANCE", "both"),
                signal_leg(2, "SBIN", "both")
            ]),
            json!({}),
        ));
        let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = vec![];
        for leg in [1, 2] {
            let (m, b, s) = (t.m.clone(), barrier.clone(), s.clone());
            handles.push(tokio::spawn(async move {
                b.wait().await;
                m.handle_signal(&s, "long_entry", Some(&json!(leg)), None, None)
                    .await
            }));
        }
        for h in handles {
            assert!(h.await.unwrap().acted());
        }
        assert_eq!(t.m.store.list_runs(sid, 10).unwrap().len(), 1);
        assert_eq!(t.gw.placed().len(), 2);
    }
}
// ===================================================================== validation

/// web: test/test_strategy_module_api.py (validator half)
mod strategy_module_validation {
    use super::*;
    use openalgo_desktop_lib::strategy::validate::validate_strategy_config;

    fn v(t: &T, cfg: Value) -> Result<Value, String> {
        validate_strategy_config(&cfg, &t.symbols.snapshot())
    }

    fn base() -> Value {
        config("V", json!([short_call_leg()]), json!({}))
    }

    #[test]
    fn a_valid_config_is_normalised_and_idempotent() {
        let t = t();
        let c = v(&t, base()).unwrap();
        assert_eq!(c["legs"][0]["risk_unit"], "points");
        assert_eq!(c["strategy_kind"], "batch");
        assert_eq!(v(&t, c.clone()).unwrap(), c);
    }

    #[test]
    fn an_unknown_field_is_refused_not_dropped() {
        let t = t();
        let mut c = base();
        c["overall_sl_mtmm"] = json!(5000);
        assert!(v(&t, c)
            .unwrap_err()
            .starts_with("The request does not accept overall_sl_mtmm"));
    }

    #[test]
    fn a_negative_loss_threshold_is_refused() {
        let t = t();
        let mut c = base();
        c["overall_sl_mtm"] = json!(-5000);
        assert!(v(&t, c)
            .unwrap_err()
            .contains("entered as a positive amount"));
    }

    #[test]
    fn a_fractional_strike_is_kept() {
        let t = t();
        let mut leg = short_call_leg();
        leg["strike_mode"] = json!("strike");
        leg.as_object_mut().unwrap().remove("atm_offset");
        leg["strike"] = json!(292.5);
        let c = v(&t, config("F", json!([leg]), json!({}))).unwrap();
        assert_eq!(c["legs"][0]["strike"], 292.5);
    }

    #[test]
    fn intraday_needs_both_times_in_order() {
        let t = t();
        let mut c = base();
        c["strategy_type"] = json!("intraday");
        assert_eq!(
            v(&t, c.clone()).unwrap_err(),
            "entry_time is required for an intraday strategy"
        );
        c["entry_time"] = json!("15:00");
        c["exit_time"] = json!("09:20");
        assert_eq!(
            v(&t, c).unwrap_err(),
            "entry_time must be earlier than exit_time"
        );
    }

    #[test]
    fn a_lock_floor_above_its_threshold_is_refused() {
        let t = t();
        let mut c = base();
        c["lock_profit"] = json!({"mode": "lock", "if_profit_reaches": 1000, "lock_profit": 2000});
        assert!(v(&t, c).unwrap_err().contains("cannot be more than"));
    }

    #[test]
    fn a_cash_leg_outside_the_stocks_tab_is_refused() {
        let t = t();
        let leg = json!({"id": 1, "segment": "cash", "position": "B", "lots": 10});
        let c = config("C", json!([leg]), json!({"universe_tab": "weekly_monthly"}));
        assert!(v(&t, c).unwrap_err().contains("does not offer"));
    }

    #[test]
    fn a_short_cash_leg_under_carry_is_refused() {
        let t = t();
        let leg = json!({"id": 1, "segment": "cash", "position": "S", "lots": 10});
        let c = config(
            "C",
            json!([leg]),
            json!({"universe_tab": "stocks_fno", "underlying": "SBIN", "underlying_exchange": "NSE"}),
        );
        assert!(v(&t, c)
            .unwrap_err()
            .contains("Cash cannot be held short overnight"));
    }

    #[test]
    fn a_signal_quantity_off_the_lot_boundary_is_refused() {
        let t = t();
        let leg = json!({"id": 1, "symbol": "RELIANCE27OCT26FUT", "exchange": "NFO", "side": "both",
                         "qty": 250, "qty_mode": "units", "segment": "futures"});
        let e = v(&t, signal_config(json!([leg]), json!({}))).unwrap_err();
        assert!(e.contains("not a whole number of lots"), "{}", e);
    }

    #[test]
    fn a_leg_side_its_direction_never_acts_on_is_refused() {
        let t = t();
        let c = signal_config(
            json!([signal_leg(1, "RELIANCE", "short")]),
            json!({"direction": "long_only"}),
        );
        assert!(v(&t, c).unwrap_err().contains("never acts on"));
    }

    #[test]
    fn only_market_is_accepted() {
        let t = t();
        let mut c = base();
        c["pricetype"] = json!("LIMIT");
        assert!(v(&t, c)
            .unwrap_err()
            .starts_with("pricetype must be one of: MARKET"));
    }

    #[test]
    fn a_bad_allowlist_entry_is_refused() {
        let t = t();
        let mut c = base();
        c["webhook_ip_allowlist"] = json!(["10.0.0.0/8", "nope"]);
        assert!(v(&t, c)
            .unwrap_err()
            .contains("is not a valid IP address or CIDR range"));
    }
}

// ===================================================================== HTTP

/// web: test/test_strategy_module_api.py, test_strategy_module_lifecycle_api.py
mod strategy_module_api {
    use super::*;
    use axum::http::{Method, StatusCode};

    fn create_body() -> Value {
        json!({
            "name": "Short straddle",
            "underlying": "NIFTY",
            "underlying_exchange": "NSE_INDEX",
            "strategy_type": "positional",
            "legs": [short_call_leg()],
        })
    }

    #[tokio::test]
    async fn create_answers_201_and_shows_the_token_once() {
        let a = app();
        let (s, b) = a.post("/strategy/api/strategies", create_body()).await;
        assert_eq!(s, StatusCode::CREATED, "{}", b);
        assert_eq!(b["status"], "success");
        let token = b["webhook_token"].as_str().unwrap().to_string();
        assert!(token.starts_with("oaws_"));
        assert!(b["message"]
            .as_str()
            .unwrap()
            .contains("Copy the webhook token now"));
        let sid = b["data"]["id"].as_i64().unwrap();
        let (s, d) = a.get(&format!("/strategy/api/strategies/{}", sid)).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!d.to_string().contains(&token));
        let (_, list) = a.get("/strategy/api/strategies").await;
        assert_eq!(list["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_session_route_needs_a_signed_in_user() {
        let a = app();
        let mut r = a.req(Method::GET, "/strategy/api/strategies", None, false);
        r.headers_mut().remove(axum::http::header::COOKIE);
        let (s, b) = a.send(r).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(b["message"], "Not authenticated");
    }

    #[tokio::test]
    async fn a_write_without_the_csrf_token_is_refused() {
        let a = app();
        let (s, _) = a
            .send(a.req(
                Method::POST,
                "/strategy/api/strategies",
                Some(create_body()),
                false,
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "CSRF refusal");
        let mut r = a.req(
            Method::POST,
            "/strategy/api/strategies",
            Some(create_body()),
            true,
        );
        r.headers_mut()
            .insert("origin", "https://evil.example".parse().unwrap());
        let (s, _) = a.send(r).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_strategy_that_is_not_yours_answers_404_not_403() {
        let a = app();
        let (row, _) = a
            .ctx
            .strategy
            .store
            .create_strategy(
                "someone-else",
                &config("Theirs", json!([short_call_leg()]), json!({})),
            )
            .unwrap();
        for path in ["", "/runs", "/orders", "/events", "/checkpoints"] {
            let (s, b) = a
                .get(&format!("/strategy/api/strategies/{}{}", row.id, path))
                .await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{}", path);
            assert_eq!(b["message"], "Strategy not found");
        }
        let (s, _) = a.get("/strategy/api/strategies/999999").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn validation_errors_are_400_with_a_message() {
        let a = app();
        let mut body = create_body();
        body["legs"] = json!([]);
        let (s, b) = a.post("/strategy/api/strategies", body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            b,
            json!({"status": "error", "message": "A strategy needs at least 1 leg"})
        );
    }

    #[tokio::test]
    async fn a_patch_revalidates_the_merged_config_and_refuses_a_kind_change() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let sid = b["data"]["id"].as_i64().unwrap();
        let path = format!("/strategy/api/strategies/{}", sid);
        let (s, b) = a
            .send(a.req(
                Method::PATCH,
                &path,
                Some(json!({"overall_sl_mtm": 2500})),
                true,
            ))
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(b["data"]["overall_sl_mtm"], 2500.0);
        let (s, b) = a
            .send(a.req(
                Method::PATCH,
                &path,
                Some(json!({"strategy_kind": "signal"})),
                true,
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(b["message"]
            .as_str()
            .unwrap()
            .contains("cannot change between batch and signal"));
        let (s, _) = a
            .send(a.req(Method::PATCH, &path, Some(json!({})), true))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, b) = a
            .send(a.req(
                Method::PATCH,
                &path,
                Some(json!({"strategy_type": "intraday"})),
                true,
            ))
            .await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "merged config re-validated: {}",
            b
        );
    }

    #[tokio::test]
    async fn start_requires_a_mode() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let sid = b["data"]["id"].as_i64().unwrap();
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/start", sid),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["message"], "mode must be one of: live, sandbox");
        let (s, _) = a
            .post(&format!("/strategy/api/strategies/{}/stop", sid), json!({}))
            .await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_sandbox_run_starts_fills_through_the_sandbox_and_stops_flat() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let sid = b["data"]["id"].as_i64().unwrap();
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/start", sid),
                json!({"mode": "sandbox"}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let run = b["run_id"].as_i64().unwrap();
        assert_eq!(b["mode"], "sandbox");
        // The sandbox fills the MARKET entry and publishes order.update; the
        // strategy subscriber folds it into the leg.
        let m = a.ctx.strategy.clone();
        assert!(
            a.until(|| m
                .state
                .snapshot(run)
                .map(|s| s.legs["1"].entry_status == "complete")
                .unwrap_or(false))
                .await
        );
        assert_eq!(m.state.snapshot(run).unwrap().legs["1"].entry_avg, 100.0);
        let (s, b) = a
            .post(&format!("/strategy/api/strategies/{}/stop", sid), json!({}))
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let store = a.ctx.strategy.store.clone();
        assert!(
            a.until(|| store.get_run(run).unwrap().unwrap().stopped_at.is_some())
                .await
        );
        let (_, orders) = a
            .get(&format!("/strategy/api/strategies/{}/orders", sid))
            .await;
        let rows = orders["data"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["action"], "BUY");
        assert_eq!(rows[1]["status"], "complete");
        // Nothing reached the live broker.
        assert!(!a.mock.calls().iter().any(|c| matches!(
            c,
            openalgo_desktop_lib::brokers::mock::MockCall::PlaceOrder(_)
        )));
        // The orderbook view reads the sandbox book for a sandbox run.
        let (s, ob) = a
            .get(&format!("/strategy/api/strategies/{}/orderbook", sid))
            .await;
        assert_eq!(s, StatusCode::OK, "{}", ob);
        assert_eq!(ob["data"]["orders"].as_array().unwrap().len(), 2);
        assert_eq!(ob["mode"], "analyze");
        let (_, cps) = a
            .get(&format!("/strategy/api/strategies/{}/checkpoints", sid))
            .await;
        assert_eq!(cps["run_id"], run);
        let (_, runs) = a
            .get(&format!("/strategy/api/strategies/{}/runs", sid))
            .await;
        assert_eq!(runs["data"][0]["stop_reason"], "manual");
    }

    #[tokio::test]
    async fn the_kill_switch_locks_and_flattens() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let sid = b["data"]["id"].as_i64().unwrap();
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/kill_switch", sid),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["webhook_locked"], true);
        assert_eq!(b["run_stopped"], false);
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/unlock_webhook", sid),
                json!({}),
            )
            .await;
        assert_eq!(
            (s, b["webhook_locked"].clone()),
            (StatusCode::OK, json!(false))
        );
    }

    #[tokio::test]
    async fn rotate_returns_a_new_token_and_live_toggles() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let sid = b["data"]["id"].as_i64().unwrap();
        let old = b["webhook_token"].as_str().unwrap().to_string();
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/webhook/rotate", sid),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        assert_ne!(b["webhook_token"].as_str().unwrap(), old);
        let (s, _) = a
            .post(&format!("/strategy/api/strategies/{}/live", sid), json!({}))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/live", sid),
                json!({"enabled": true}),
            )
            .await;
        assert_eq!(
            (s, b["live_enabled"].clone()),
            (StatusCode::OK, json!(true))
        );
    }

    #[tokio::test]
    async fn the_events_query_is_validated_and_clamped() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let sid = b["data"]["id"].as_i64().unwrap();
        let (s, _) = a
            .get(&format!(
                "/strategy/api/strategies/{}/events?kind=bogus",
                sid
            ))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = a
            .get(&format!("/strategy/api/strategies/{}/events?limit=x", sid))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, b) = a
            .get(&format!("/strategy/api/strategies/{}/events?limit=-1", sid))
            .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["data"].as_array().unwrap().len(), 1, "clamped to 1");
    }

    #[tokio::test]
    async fn the_public_webhook_needs_no_session_or_csrf() {
        let a = app();
        let (_, b) = a.post("/strategy/api/strategies", create_body()).await;
        let token = b["webhook_token"].as_str().unwrap().to_string();
        let (s, b) = a
            .webhook(&token, r#"{"action":"start","mode":"sandbox"}"#)
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(b["result"], "ok");
        let (s, b) = a.webhook("oaws_unknown_but_long_enough_xx", "{}").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(
            b,
            json!({"status": "error", "result": "rejected_token", "message": "Unknown or expired webhook token"})
        );
        let big = format!("{{\"x\":\"{}\"}}", "a".repeat(17000));
        let (s, _) = a.webhook(&token, &big).await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    }
}

/// web: test/test_strategy_restx_api.py
mod strategy_restx_api {
    use super::*;
    use axum::http::StatusCode;

    async fn made(a: &App) -> i64 {
        let (_, b) = a
            .post(
                "/strategy/api/strategies",
                json!({"name": "API", "underlying": "NIFTY", "underlying_exchange": "NSE_INDEX",
                       "strategy_type": "positional", "legs": [short_call_leg()]}),
            )
            .await;
        b["data"]["id"].as_i64().unwrap()
    }

    #[tokio::test]
    async fn mode_is_required_on_start_and_never_defaulted() {
        let a = app();
        let sid = made(&a).await;
        let (s, b) = a
            .api("/api/v1/strategy/start", json!({"strategy_id": sid}))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["message"]["mode"][0], "Missing data for required field.");
        let (s, b) = a
            .api(
                "/api/v1/strategy/start",
                json!({"strategy_id": sid, "mode": "paper"}),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["message"]["mode"][0], "Must be one of: live, sandbox.");
    }

    #[tokio::test]
    async fn live_without_the_opt_in_is_a_409() {
        let a = app();
        let sid = made(&a).await;
        let (s, b) = a
            .api(
                "/api/v1/strategy/start",
                json!({"strategy_id": sid, "mode": "live"}),
            )
            .await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(b["message"]
            .as_str()
            .unwrap()
            .contains("not enabled for live trading"));
    }

    #[tokio::test]
    async fn an_invalid_key_is_403_and_a_missing_key_is_400() {
        let a = app();
        let (s, b) = a
            .send(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/strategy/list")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"apikey": "wrong"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            (s, b["message"].clone()),
            (StatusCode::FORBIDDEN, json!("Invalid openalgo apikey"))
        );
        let (s, b) = a
            .send(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/strategy/list")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from("not json"))
                    .unwrap(),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            b["message"]["apikey"][0],
            "Missing data for required field."
        );
    }

    #[tokio::test]
    async fn list_status_runs_orders_and_events_have_the_web_shapes() {
        let a = app();
        let sid = made(&a).await;
        let (s, b) = a.api("/api/v1/strategy/list", json!({})).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["data"][0]["id"], sid);
        let (_, b) = a
            .api("/api/v1/strategy/status", json!({"strategy_id": sid}))
            .await;
        assert_eq!(b["status"], "success");
        assert_eq!(b["run"], Value::Null);
        assert!(b["data"]["legs"].is_array());
        assert!(!b.to_string().contains("oaws_"));
        let (s, b) = a
            .api(
                "/api/v1/strategy/start",
                json!({"strategy_id": sid, "mode": "sandbox"}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        let run = b["run_id"].as_i64().unwrap();
        let (_, b) = a
            .api(
                "/api/v1/strategy/runs",
                json!({"strategy_id": sid, "limit": 5}),
            )
            .await;
        assert_eq!(b["data"][0]["id"], run);
        let (_, b) = a
            .api(
                "/api/v1/strategy/orders",
                json!({"strategy_id": sid, "run_id": run}),
            )
            .await;
        assert_eq!(b["data"][0]["kind"], "entry");
        let (s, b) = a
            .api(
                "/api/v1/strategy/events",
                json!({"strategy_id": sid, "limit": 0}),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            b["message"]["limit"][0],
            "Must be greater than or equal to 1 and less than or equal to 1000."
        );
        let (_, b) = a
            .api(
                "/api/v1/strategy/events",
                json!({"strategy_id": sid, "kind": "run_started"}),
            )
            .await;
        assert_eq!(b["data"].as_array().unwrap().len(), 1);
        // The sandbox entry filled at once: the leg closes, then a second
        // close finds nothing open.
        let (s, b) = a
            .api(
                "/api/v1/strategy/close_leg",
                json!({"strategy_id": sid, "leg_id": 1}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(
            (b["run_id"].as_i64(), b["leg_id"].as_i64()),
            (Some(run), Some(1))
        );
        let (s, _) = a
            .api(
                "/api/v1/strategy/close_leg",
                json!({"strategy_id": sid, "leg_id": 1}),
            )
            .await;
        assert_eq!(s, StatusCode::CONFLICT, "already closed");
    }

    #[tokio::test]
    async fn another_users_strategy_is_404() {
        let a = app();
        let (row, _) = a
            .ctx
            .strategy
            .store
            .create_strategy(
                "someone-else",
                &config("X", json!([short_call_leg()]), json!({})),
            )
            .unwrap();
        let (s, b) = a
            .api("/api/v1/strategy/status", json!({"strategy_id": row.id}))
            .await;
        assert_eq!(
            (s, b["message"].clone()),
            (StatusCode::NOT_FOUND, json!("Strategy not found"))
        );
        let (s, _) = a
            .api("/api/v1/strategy/stop", json!({"strategy_id": row.id}))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stop_and_close_all_on_a_stopped_strategy_are_409() {
        let a = app();
        let sid = made(&a).await;
        for (p, body) in [
            ("stop", json!({"strategy_id": sid})),
            ("close_all", json!({"strategy_id": sid})),
            ("close_leg", json!({"strategy_id": sid, "leg_id": 1})),
        ] {
            let (s, b) = a.api(&format!("/api/v1/strategy/{}", p), body).await;
            assert_eq!(s, StatusCode::CONFLICT, "{}", p);
            assert_eq!(b["message"], "This strategy is not running");
        }
    }
}

// ===================================================================== live mode

/// force_live and per-run destinations against a full app context.
mod live_mode {
    use super::*;
    use openalgo_desktop_lib::brokers::mock::MockCall;
    use openalgo_desktop_lib::strategy::engine::FillOpts;

    fn live_places(a: &App) -> Vec<String> {
        a.mock
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                MockCall::PlaceOrder(o) => Some(format!("{} {}", o.action.as_str(), o.symbol)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_analyzer_toggle_mid_run_does_not_send_a_live_runs_exits_to_the_sandbox() {
        let a = app();
        let cfg = config("Live", json!([short_call_leg()]), json!({}));
        let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
        a.ctx
            .strategy
            .store
            .set_live_enabled(row.id, USER, true)
            .unwrap();
        a.ctx.sqlite.set_analyze_mode(false).unwrap();
        let r = a
            .ctx
            .strategy
            .start_run(row.id, USER, "live", "manual", None)
            .await;
        assert!(r.ok, "{:?}", r);
        let run = r.run_id.unwrap();
        a.ctx
            .strategy
            .apply_fill(run, 1, Some(100.0), true, FillOpts::default())
            .await;
        // The operator switches analyzer mode on while the live run holds a
        // real position.
        a.ctx.sqlite.set_analyze_mode(true).unwrap();
        let out = a.ctx.strategy.stop_run(run, USER, "manual").await;
        assert!(out.ok, "{:?}", out);
        assert_eq!(
            live_places(&a),
            vec![format!("SELL {}", ATM_CE), format!("BUY {}", ATM_CE)]
        );
        let sandbox_orders = a.ctx.sandbox.orderbook().await.unwrap();
        let v = serde_json::to_value(&sandbox_orders).unwrap();
        assert_eq!(
            v["data"]["orders"].as_array().map(|o| o.len()).unwrap_or(0),
            0
        );
    }

    #[tokio::test]
    async fn a_sandbox_run_never_reaches_the_broker_with_analyzer_off() {
        let a = app();
        a.ctx.sqlite.set_analyze_mode(false).unwrap();
        let cfg = config("Sbx", json!([short_call_leg()]), json!({}));
        let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
        let r = a
            .ctx
            .strategy
            .start_run(row.id, USER, "sandbox", "manual", None)
            .await;
        assert!(r.ok, "{:?}", r);
        assert!(live_places(&a).is_empty());
    }

    #[tokio::test]
    async fn a_live_order_bypasses_the_action_center_queue() {
        let a = app();
        openalgo_desktop_lib::services::apikey_service::ApiKeyService::set_order_mode(
            &a.ctx,
            "semi_auto",
        )
        .unwrap();
        let cfg = config("Semi", json!([short_call_leg()]), json!({}));
        let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
        a.ctx
            .strategy
            .store
            .set_live_enabled(row.id, USER, true)
            .unwrap();
        let r = a
            .ctx
            .strategy
            .start_run(row.id, USER, "live", "manual", None)
            .await;
        assert!(r.ok, "{:?}", r);
        assert_eq!(live_places(&a).len(), 1);
    }

    #[tokio::test]
    async fn a_live_start_without_a_broker_session_is_refused_before_anything_is_claimed() {
        let a = app();
        a.ctx.set_broker_session(None);
        let cfg = config("NoSess", json!([short_call_leg()]), json!({}));
        let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
        a.ctx
            .strategy
            .store
            .set_live_enabled(row.id, USER, true)
            .unwrap();
        let r = a
            .ctx
            .strategy
            .start_run(row.id, USER, "live", "manual", None)
            .await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("Broker session"));
        assert_eq!(
            a.ctx
                .strategy
                .store
                .get_strategy(row.id, USER)
                .unwrap()
                .unwrap()
                .status,
            "stopped"
        );
    }
}

// ===================================================================== strategy book

/// web: subscribers/strategy_book_subscriber.py, test_strategy_book_prune_lock.py
mod strategy_book {
    use super::*;
    use openalgo_desktop_lib::events::{Event, Mode, OrderMeta, OrderUpdate};

    fn placed(orderid: &str, strategy: &str) -> Event {
        Event::OrderPlaced {
            meta: OrderMeta {
                mode: Mode::Live,
                api_type: "placeorder".into(),
                request_data: json!({}),
                response_data: json!({}),
            },
            strategy: strategy.into(),
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            quantity: 10,
            pricetype: "MARKET".into(),
            product: "MIS".into(),
            orderid: orderid.into(),
        }
    }

    fn fill(orderid: &str, action: &str, qty: i64, price: f64, status: &str) -> Event {
        Event::OrderUpdate(OrderUpdate {
            orderid: orderid.into(),
            action: action.into(),
            order_status: status.into(),
            filled_quantity: qty,
            average_price: price,
            ..Default::default()
        })
    }

    async fn legs(a: &App) -> Vec<Value> {
        a.ctx
            .strategy
            .book
            .get_strategy_legs(None, Some("MyAlgo"))
            .unwrap()
    }

    #[tokio::test]
    async fn tags_and_fills_book_a_position_per_strategy() {
        let a = app();
        a.ctx.bus.publish(placed("O1", "MyAlgo"));
        a.ctx.bus.publish(fill("O1", "BUY", 10, 500.0, "complete"));
        let book = a.ctx.strategy.book.clone();
        assert!(
            a.until(|| book.get_strategy_legs(None, Some("MyAlgo")).unwrap().len() == 1)
                .await
        );
        let l = &legs(&a).await[0];
        assert_eq!(
            (l["quantity"].as_f64(), l["average_price"].as_f64()),
            (Some(10.0), Some(500.0))
        );
        // A duplicate fill books nothing; the closing fill realizes.
        a.ctx.bus.publish(fill("O1", "BUY", 10, 500.0, "complete"));
        a.ctx.bus.publish(placed("O2", "MyAlgo"));
        a.ctx.bus.publish(fill("O2", "SELL", 10, 510.0, "complete"));
        assert!(
            a.until(|| book.get_strategy_legs(None, Some("MyAlgo")).unwrap()[0]["quantity"] == 0.0)
                .await
        );
        let l = &legs(&a).await[0];
        assert_eq!(l["realized_pnl"].as_f64(), Some(100.0));
    }

    #[tokio::test]
    async fn a_fill_that_beats_its_tag_is_buffered_and_drained() {
        let a = app();
        let book = &a.ctx.strategy.book;
        assert!(book.apply_fill("O9", 5.0, 100.0, "BUY").unwrap().is_none());
        assert_eq!(book.pending_fill_count().unwrap(), 1);
        book.record_order_tag("O9", "", "MyAlgo", "SBIN", "NSE", "MIS")
            .unwrap();
        assert_eq!(book.pending_fill_count().unwrap(), 0);
        let l = book.get_strategy_legs(None, Some("MyAlgo")).unwrap();
        assert_eq!(l[0]["quantity"].as_f64(), Some(5.0));
    }

    #[tokio::test]
    async fn partials_are_priced_from_the_change_in_notional() {
        let a = app();
        let book = &a.ctx.strategy.book;
        book.record_order_tag("P1", "", "MyAlgo", "SBIN", "NSE", "MIS")
            .unwrap();
        book.apply_fill("P1", 4.0, 100.0, "BUY").unwrap();
        book.apply_fill("P1", 10.0, 103.0, "BUY").unwrap(); // last 6 at 105
        let l = book.get_strategy_legs(None, Some("MyAlgo")).unwrap();
        assert!((l[0]["average_price"].as_f64().unwrap() - 103.0).abs() < 1e-9);
        assert_eq!(l[0]["quantity"].as_f64(), Some(10.0));
    }

    #[tokio::test]
    async fn an_untagged_order_never_books() {
        let a = app();
        let book = &a.ctx.strategy.book;
        book.record_order_tag("T1", "", "", "SBIN", "NSE", "MIS")
            .unwrap();
        assert!(book.get_strategy_legs(None, None).unwrap().is_empty());
    }
}

// ===================================================================== webhook security

/// The public webhook can place real orders: per-address limits, per-webhook
/// lockout, nothing throttled or locked reaches the order path.
mod webhook_security {
    use super::*;
    use axum::http::StatusCode;
    use openalgo_desktop_lib::strategy::webhook::LOCKOUT_FAILURES;

    const START: &str = r#"{"action":"start","mode":"sandbox"}"#;

    async fn create(a: &App, allowlist: Value) -> (i64, String) {
        let (s, b) = a
            .post(
                "/strategy/api/strategies",
                json!({
                    "name": "Guarded",
                    "underlying": "NIFTY",
                    "underlying_exchange": "NSE_INDEX",
                    "strategy_type": "positional",
                    "legs": [short_call_leg()],
                    "webhook_ip_allowlist": allowlist,
                }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{}", b);
        (
            b["data"]["id"].as_i64().unwrap(),
            b["webhook_token"].as_str().unwrap().to_string(),
        )
    }

    fn audit_rows(a: &App, sid: i64) -> usize {
        a.ctx
            .strategy
            .store
            .list_webhook_events(sid, 1000)
            .unwrap()
            .len()
    }

    async fn orders(a: &App, sid: i64) -> usize {
        let (_, b) = a
            .get(&format!("/strategy/api/strategies/{}/orders", sid))
            .await;
        b["data"].as_array().map(|v| v.len()).unwrap_or(0)
    }

    fn locked(a: &App, sid: i64) -> bool {
        a.ctx
            .strategy
            .store
            .get_strategy(sid, USER)
            .unwrap()
            .unwrap()
            .webhook_locked
    }

    #[tokio::test]
    async fn a_burst_over_the_limit_is_refused_before_any_check_or_order() {
        let a = app();
        let (sid, token) = create(&a, json!([])).await;
        // 100 per minute (web WEBHOOK_RATE_LIMIT); each reaches the pipeline
        // and is refused there for its payload, so no order is placed.
        for _ in 0..100 {
            let (s, b) = a.webhook_from("198.51.100.1", &token, "{}").await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{}", b);
        }
        assert_eq!(audit_rows(&a, sid), 100);
        // Over the limit: refused before the token is even looked up, even
        // for a well-formed start alert.
        let (s, b) = a.webhook_from("198.51.100.1", &token, START).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(b["result"], "rate_limited");
        let (s, _) = a.webhook_from("198.51.100.1", "not-a-token", START).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(audit_rows(&a, sid), 100, "no audit row, no lookup");
        assert_eq!(orders(&a, sid).await, 0);
        assert!(a.ctx.strategy.store.list_runs(sid, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn bad_attempts_lock_the_webhook_until_the_trader_unlocks_it() {
        let a = app();
        let (sid, token) = create(&a, json!(["10.0.0.0/8"])).await;
        for i in 0..LOCKOUT_FAILURES {
            let (s, b) = a.webhook_from("203.0.113.9", &token, START).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "attempt {}: {}", i, b);
            assert_eq!(b["result"], "rejected_ip");
        }
        assert!(locked(&a, sid));
        // The right caller with the right token is refused while locked.
        let (s, b) = a.webhook_from("10.0.0.7", &token, START).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{}", b);
        assert_eq!(b["result"], "rejected_locked");
        assert_eq!(orders(&a, sid).await, 0);
        let (_, ev) = a
            .get(&format!("/strategy/api/strategies/{}/events", sid))
            .await;
        assert!(
            ev.to_string().contains("webhook_locked"),
            "a critical event tells the trader: {}",
            ev
        );

        // Unlock restores it.
        let (s, b) = a
            .post(
                &format!("/strategy/api/strategies/{}/unlock_webhook", sid),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert!(!locked(&a, sid));
        let (s, b) = a.webhook_from("10.0.0.7", &token, START).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(b["result"], "ok");
    }

    #[tokio::test]
    async fn rotating_source_addresses_still_trips_the_per_webhook_lockout() {
        let a = app();
        let (sid, token) = create(&a, json!(["10.0.0.0/8"])).await;
        for i in 0..LOCKOUT_FAILURES {
            let ip = format!("192.0.2.{}", i + 1);
            let (s, _) = a.webhook_from(&ip, &token, START).await;
            assert_eq!(s, StatusCode::FORBIDDEN);
        }
        assert!(locked(&a, sid));
        let (s, b) = a.webhook_from("10.1.2.3", &token, START).await;
        assert_eq!(b["result"], "rejected_locked", "{}", b);
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(orders(&a, sid).await, 0);
    }

    #[tokio::test]
    async fn guessing_tokens_from_one_address_gets_that_address_refused() {
        let a = app();
        let (sid, token) = create(&a, json!([])).await;
        for i in 0..10 {
            let guess = format!("oaws_{:0>43}", i);
            let (s, _) = a.webhook_from("198.51.100.77", &guess, "{}").await;
            assert_eq!(s, StatusCode::NOT_FOUND);
        }
        // Even the real token is now refused from that address, before any
        // lookup; another address is unaffected.
        let (s, b) = a.webhook_from("198.51.100.77", &token, START).await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{}", b);
        assert_eq!(audit_rows(&a, sid), 0);
        let (s, b) = a.webhook_from("198.51.100.78", &token, START).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
    }

    #[tokio::test]
    async fn a_valid_alert_within_limits_places_exactly_one_order() {
        let a = app();
        let (sid, token) = create(&a, json!(["10.0.0.0/8"])).await;
        let (s, b) = a.webhook_from("10.0.0.7", &token, START).await;
        assert_eq!(s, StatusCode::OK, "{}", b);
        assert_eq!(b["result"], "ok");
        let store = a.ctx.strategy.store.clone();
        assert!(
            a.until(|| store
                .list_orders_for_strategy(sid, None)
                .map(|v| !v.is_empty())
                .unwrap_or(false))
                .await
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(orders(&a, sid).await, 1);
        assert!(!locked(&a, sid));
    }
}

// ===================================================================== hygiene

/// Owned tasks, bounded registries, subscriptions released.
mod hygiene {
    use super::*;
    use openalgo_desktop_lib::strategy::tick_feed::{Key, PriceSource, TickFeed};
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Default)]
    struct Prices {
        subs: Mutex<Vec<Key>>,
        unsubs: Mutex<Vec<Key>>,
        tx: Mutex<
            Option<
                tokio::sync::broadcast::Sender<
                    openalgo_desktop_lib::brokers::common::streaming::MarketEvent,
                >,
            >,
        >,
    }

    #[async_trait::async_trait]
    impl PriceSource for Prices {
        async fn subscribe(&self, keys: &[Key]) {
            self.subs.lock().extend_from_slice(keys);
        }
        async fn unsubscribe(&self, keys: &[Key]) {
            self.unsubs.lock().extend_from_slice(keys);
        }
        fn ticks(
            &self,
        ) -> tokio::sync::broadcast::Receiver<
            openalgo_desktop_lib::brokers::common::streaming::MarketEvent,
        > {
            let mut g = self.tx.lock();
            if g.is_none() {
                *g = Some(tokio::sync::broadcast::channel(64).0);
            }
            g.as_ref().unwrap().subscribe()
        }
        async fn poll(&self, _keys: &[Key]) -> Vec<(Key, f64)> {
            vec![]
        }
    }

    #[tokio::test]
    async fn subscriptions_are_refcounted_per_run_and_released() {
        let p = Arc::new(Prices::default());
        let feed = TickFeed::new(Some(p.clone()));
        let k = ("X".to_string(), "NFO".to_string());
        feed.add_run(1, std::slice::from_ref(&k)).await;
        feed.add_run(2, std::slice::from_ref(&k)).await;
        assert_eq!(p.subs.lock().len(), 1, "one subscription for two runs");
        feed.remove_run(1).await;
        assert!(p.unsubs.lock().is_empty());
        feed.remove_run(2).await;
        assert_eq!(p.unsubs.lock().len(), 1);
        assert!(feed.subscribed().is_empty());
        assert_eq!(feed.runs_tracked(), 0);
    }

    #[tokio::test]
    async fn background_tasks_are_owned_and_stopped() {
        let t = t();
        t.m.start().await;
        assert!(t.m.task_count() >= 2, "checkpoint and scheduler tasks");
        t.m.shutdown().await;
        assert_eq!(t.m.task_count(), 0);
    }

    #[tokio::test]
    async fn the_app_shutdown_stops_the_strategy_module() {
        let a = app();
        a.ctx.strategy.start().await;
        assert!(
            a.ctx.strategy.task_count() >= 3,
            "tick consumer, checkpoint, scheduler"
        );
        a.ctx.shutdown().await;
        assert_eq!(a.ctx.strategy.task_count(), 0);
    }

    #[tokio::test]
    async fn a_hundred_runs_leave_no_state_locks_or_throttle_entries() {
        let t = t();
        t.rooms
            .watching
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let sid = t.default_strategy();
        for _ in 0..100 {
            let run = t.start_filled(sid, 100.0).await;
            t.m.process_tick(ATM_CE, "NFO", 101.0).await;
            t.m.stop_run(run, USER, "manual").await;
            t.fill_last_exit(run, 100.0).await;
            assert!(t.run(run).stopped_at.is_some());
            t.m.webhook.advance(std::time::Duration::from_secs(31));
        }
        assert!(t.m.state.is_empty());
        assert_eq!(t.m.broadcast.tracked(), 0);
        assert_eq!(t.m.feed.runs_tracked(), 0);
        assert!(t.m.order_events.is_empty());
    }
}
