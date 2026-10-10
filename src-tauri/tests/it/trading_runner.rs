//! The OpenScript live runner with a fake host and fake services: lifecycle
//! (start, pause, close), status shapes, orders tagged and booked per
//! deployment, sandbox versus live, schedules on the injected IST clock,
//! teardown on logout and shutdown, and no task or page left behind.

use crate::webui_support::{get, ist, req, with, H};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::NaiveDate;
use openalgo_desktop_lib::strategy::dispatch::{Book, DispatchResult, OrderStatusResult, RunMode};
use openalgo_desktop_lib::trading::runner::host::{fragment_of, RecordingHost, RunnerHost};
use openalgo_desktop_lib::trading::runner::services::{Bar, RunnerServices};
use openalgo_desktop_lib::trading::runner::RunnerOptions;
use openalgo_desktop_lib::trading::scripts::source_hash;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// One order the fake destination holds. An id belongs to one side: the
/// sandbox and the broker number their orders separately, so asking one side
/// about the other's id finds nothing, as it would in the app.
struct FakeOrder {
    mode: RunMode,
    id: String,
    status: Value,
}

#[derive(Default)]
struct Fake {
    analyzer: AtomicBool,
    broker: AtomicBool,
    refuse_orders: AtomicBool,
    fill_at_once: AtomicBool,
    closed_market: AtomicBool,
    nobody: AtomicBool,
    unreadable_positions: AtomicBool,
    next: AtomicU64,
    placed: Mutex<Vec<(RunMode, Value, bool)>>,
    orders: Mutex<Vec<FakeOrder>>,
    cancelled: Mutex<Vec<(RunMode, String)>>,
    /// Size held outside any strategy, per side and symbol (NSE, MIS),
    /// added to what the position book reports.
    outside: Mutex<Vec<(RunMode, String, i64)>>,
}

impl Fake {
    /// Change what one side says about one of its orders.
    fn set_status(&self, mode: RunMode, id: &str, fields: Value) {
        let mut orders = self.orders.lock();
        let o = orders
            .iter_mut()
            .find(|o| o.mode == mode && o.id == id)
            .expect("no such order on that side");
        for (k, v) in fields.as_object().unwrap() {
            o.status[k] = v.clone();
        }
    }

    fn hold_outside(&self, mode: RunMode, symbol: &str, quantity: i64) {
        let mut outside = self.outside.lock();
        outside.retain(|(m, s, _)| !(*m == mode && s == symbol));
        outside.push((mode, symbol.to_string(), quantity));
    }

    fn placed_on(&self, mode: RunMode) -> Vec<Value> {
        self.placed
            .lock()
            .iter()
            .filter(|(m, _, _)| *m == mode)
            .map(|(_, r, _)| r.clone())
            .collect()
    }

    /// One side's position book: the net of its own fills, plus whatever is
    /// held there outside any strategy.
    fn positions(&self, mode: RunMode) -> Vec<Value> {
        let mut net: HashMap<(String, String, String), i64> = HashMap::new();
        for o in self.orders.lock().iter().filter(|o| o.mode == mode) {
            let s = &o.status;
            let filled = if s["order_status"] == "complete" {
                s["filled_quantity"]
                    .as_i64()
                    .or(s["quantity"].as_i64())
                    .unwrap_or(0)
            } else {
                s["filled_quantity"].as_i64().unwrap_or(0)
            };
            let side = if s["action"] == "SELL" { -1 } else { 1 };
            let key = (
                s["symbol"].as_str().unwrap_or_default().to_string(),
                s["exchange"].as_str().unwrap_or_default().to_string(),
                s["product"].as_str().unwrap_or_default().to_string(),
            );
            *net.entry(key).or_default() += side * filled;
        }
        for (m, symbol, q) in self.outside.lock().iter() {
            if *m == mode {
                *net
                    .entry((symbol.clone(), "NSE".into(), "MIS".into()))
                    .or_default() += q;
            }
        }
        net.into_iter()
            .map(|((symbol, exchange, product), quantity)| {
                json!({"symbol": symbol, "exchange": exchange, "product": product,
                       "quantity": quantity, "ltp": 110.0})
            })
            // A contract the account holds that no strategy traded.
            .chain(std::iter::once(
                json!({"symbol": "INFY", "exchange": "NSE", "product": "MIS", "quantity": 5, "ltp": 1.0}),
            ))
            .collect()
    }
}

#[async_trait]
impl RunnerServices for Fake {
    fn analyzer_on(&self) -> bool {
        self.analyzer.load(Ordering::SeqCst)
    }
    fn authorised(&self, mode: RunMode) -> Result<(), String> {
        if mode == RunMode::Live && !self.broker.load(Ordering::SeqCst) {
            return Err("Log in to your broker.".into());
        }
        Ok(())
    }
    async fn history(
        &self,
        _s: &str,
        _e: &str,
        _i: &str,
        _a: NaiveDate,
        _b: NaiveDate,
    ) -> Result<Vec<Bar>, String> {
        Ok((0..3)
            .map(|i| Bar {
                time: 1_759_635_900_000 + i * 60_000,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                volume: 10.0,
                oi: 0.0,
            })
            .collect())
    }
    async fn place(&self, mode: RunMode, request: &Value, internal: bool) -> DispatchResult {
        self.placed.lock().push((mode, request.clone(), internal));
        if self.refuse_orders.load(Ordering::SeqCst) {
            return DispatchResult::refused("Insufficient funds");
        }
        let id = format!("OID{}", self.next.fetch_add(1, Ordering::SeqCst) + 1);
        let mut status = json!({"order_status": "open", "quantity": request["quantity"],
            "action": request["action"], "symbol": request["symbol"],
            "exchange": request["exchange"], "product": request["product"]});
        if self.fill_at_once.load(Ordering::SeqCst) {
            status["order_status"] = json!("complete");
            status["average_price"] = json!(100.0);
        }
        self.orders.lock().push(FakeOrder {
            mode,
            id: id.clone(),
            status,
        });
        DispatchResult {
            ok: true,
            broker_order_id: Some(id.clone()),
            response: json!({"status": "success", "orderid": id}),
            error: None,
        }
    }
    async fn cancel(&self, mode: RunMode, orderid: &str) -> DispatchResult {
        self.cancelled.lock().push((mode, orderid.to_string()));
        let mut orders = self.orders.lock();
        let Some(o) = orders
            .iter_mut()
            .find(|o| o.mode == mode && o.id == orderid)
        else {
            return DispatchResult::refused("No such order");
        };
        o.status["order_status"] = json!("cancelled");
        DispatchResult {
            ok: true,
            broker_order_id: Some(orderid.into()),
            response: Value::Null,
            error: None,
        }
    }
    async fn order_status(&self, mode: RunMode, orderid: &str) -> OrderStatusResult {
        match self
            .orders
            .lock()
            .iter()
            .find(|o| o.mode == mode && o.id == orderid)
        {
            Some(o) => OrderStatusResult {
                ok: true,
                order: o.status.clone(),
                error: None,
            },
            None => OrderStatusResult::default(),
        }
    }
    async fn book(&self, mode: RunMode, book: Book) -> Result<Value, Value> {
        let rows: Vec<Value> = self
            .orders
            .lock()
            .iter()
            .filter(|o| o.mode == mode)
            .map(|o| {
                json!({"orderid": o.id, "symbol": o.status["symbol"], "exchange": o.status["exchange"],
                       "action": o.status["action"], "order_status": o.status["order_status"], "ltp": 110.0})
            })
            .chain(std::iter::once(json!({"orderid": "MANUAL", "symbol": "SBIN", "exchange": "NSE", "action": "BUY", "order_status": "complete"})))
            .collect();
        Ok(match book {
            Book::Orders => {
                json!({"status": "success", "data": {"orders": rows, "statistics": {"total_buy_orders": 9}}})
            }
            Book::Trades => json!({"status": "success", "data": rows}),
            Book::Positions => {
                if self.unreadable_positions.load(Ordering::SeqCst) {
                    return Err(json!({"status": "error", "message": "Session expired"}));
                }
                json!({"status": "success", "total_pnl": 999.0, "data": self.positions(mode)})
            }
        })
    }
    async fn holdings(&self, _mode: RunMode) -> Result<Value, Value> {
        Ok(json!({"status": "success", "data": {"holdings": [], "statistics": {}}}))
    }
    fn lot_size(&self, _s: &str, _e: &str) -> Option<i64> {
        Some(1)
    }
    fn is_trading_day(&self, _e: &str, _d: NaiveDate) -> bool {
        !self.closed_market.load(Ordering::SeqCst)
    }
    fn instrument(&self, _s: &str, exchange: &str, _d: NaiveDate) -> Value {
        json!({"symbol": "SBIN", "contractFound": true, "instrument": {"exchange": exchange, "timezone": "Asia/Kolkata"}, "today": null})
    }
    fn signed_in(&self) -> bool {
        !self.nobody.load(Ordering::SeqCst)
    }
}

struct T {
    h: H,
    cookie: String,
    csrf: String,
    host: Arc<RecordingHost>,
    fake: Arc<Fake>,
}

fn fast() -> RunnerOptions {
    RunnerOptions {
        heartbeat_timeout: Duration::from_secs(60),
        order_poll: Duration::from_secs(3600),
        bar_poll: Duration::from_secs(3600),
        close_wait: Duration::from_millis(300),
        close_look: Duration::from_millis(10),
        inbox_wait: Duration::from_millis(50),
        schedule_tick: Duration::from_secs(15),
        history_days: 5,
    }
}

async fn setup() -> T {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    let host = Arc::new(RecordingHost::new());
    let fake = Arc::new(Fake::default());
    fake.analyzer.store(true, Ordering::SeqCst);
    let runner = &h.ctx.trading.runner;
    runner.set_host(host.clone());
    runner.set_services(fake.clone());
    runner.set_options(fast());
    T {
        h,
        cookie,
        csrf,
        host,
        fake,
    }
}

impl T {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let token = if method == Method::GET {
            None
        } else {
            Some(self.csrf.as_str())
        };
        self.h
            .json(with(req(method, path, body), &self.cookie, token))
            .await
    }

    /// Save a script with its program and deploy it; answers the deployment.
    async fn deploy(&self, symbol: &str) -> String {
        let source = "version 1\nstrategy(\"t\", qty = 1)\nif close > open\n    buy()\n";
        let program = json!({"source": {"hash": source_hash(source)}}).to_string();
        let (s, _) = self
            .call(
                Method::POST,
                "/openscript/t.oscript",
                Some(json!({"source": source, "program": program})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        let (s, v) = self
            .call(
                Method::POST,
                "/openscript/runner/config/t.oscript",
                Some(json!({"symbol": symbol, "exchange": "NSE", "interval": "1m", "product": "MIS"})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        v["settings"]["deployment"].as_str().unwrap().to_string()
    }

    fn token(&self, run: &str) -> String {
        fragment_of(&self.host.last_launch(run).unwrap().page)
            .unwrap()
            .1
    }

    async fn page(
        &self,
        method: Method,
        run: &str,
        tail: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(format!("/openscript/runner/host/{}/{}", run, tail))
            .header("x-runner-token", self.token(run));
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        self.h.json(b.body(body).unwrap()).await
    }

    async fn buy(&self, run: &str, intent: i64, qty: i64) -> Value {
        self.order(run, intent, "buy", qty).await
    }

    async fn order(&self, run: &str, intent: i64, side: &str, qty: i64) -> Value {
        let (s, v) = self
            .page(
                Method::POST,
                run,
                "intents",
                Some(json!({"confirmed": true, "intents": [{
                    "intentId": intent, "kind": "place", "side": side, "qty": qty, "qtyType": "units",
                    "type": "market", "tag": "long", "instrument": {"symbol": "SBIN", "exchange": "NSE"},
                    "product": "", "positionRef": 1, "bar": {"index": 3, "time": 0}
                }]})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        v
    }

    async fn start(&self, run: &str) -> Value {
        let (s, v) = self
            .call(
                Method::POST,
                &format!("/openscript/runner/start/{}", run),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{}", v);
        v
    }

    async fn close(&self, run: &str) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            &format!("/openscript/runner/close/{}", run),
            None,
        )
        .await
    }

    /// Run on the sandbox side, trade, let it fill, then end the run in a way
    /// that leaves the position where it is (Pause, or the pause every run
    /// gets on logout and on quit).
    async fn leave_a_sandbox_position(&self, run: &str, side: &str, qty: i64, by_logout: bool) {
        self.fake.analyzer.store(true, Ordering::SeqCst);
        self.fake.fill_at_once.store(true, Ordering::SeqCst);
        assert_eq!(self.start(run).await["run"]["mode"], "sandbox");
        self.order(run, 1, side, qty).await;
        let runner = &self.h.ctx.trading.runner;
        runner.pump_once(run).await;
        if by_logout {
            assert_eq!(runner.pause_all("Paused because you signed out").len(), 1);
        } else {
            runner.pause(run).unwrap();
        }
        assert!(!runner.is_running(run));
        assert_eq!(self.fake.placed_on(RunMode::Sandbox).len(), 1);
    }

    /// Switch the platform to live with a broker, and start the run there.
    async fn go_live(&self, run: &str) {
        self.fake.analyzer.store(false, Ordering::SeqCst);
        self.fake.broker.store(true, Ordering::SeqCst);
        assert_eq!(self.start(run).await["run"]["mode"], "live");
    }
}

#[tokio::test]
async fn start_page_orders_books_and_pause() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    assert!(id.starts_with("openscript_t_SBIN_NSE_1m_"));

    // A start that carries options is refused, not ignored.
    let (s, _) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            Some(json!({"mode": "live"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{}", v);
    assert_eq!(v["run"]["id"], id);
    assert_eq!(v["run"]["deployment"], id);
    assert_eq!(v["run"]["file"], "t.oscript");
    assert_eq!(v["run"]["state"], "running");
    assert_eq!(v["run"]["mode"], "sandbox");
    assert!(t.host.is_open(&id));
    let launch = t.host.last_launch(&id).unwrap();
    assert!(launch.page.starts_with("/openscript-runner.html#run="));

    let (s, _) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "one run per deployment");

    let (s, v) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["running"].as_array().unwrap().len(), 1);
    assert_eq!(v["settings"][0]["deployment"], id);
    assert!(v["log_dir"].as_str().unwrap().ends_with("openscript_logs"));
    let (_, one) = t
        .call(
            Method::GET,
            &format!("/openscript/runner/status/{}", id),
            None,
        )
        .await;
    assert_eq!(one["running"], true);
    assert_eq!(one["logs"].as_array().unwrap().len(), 1);

    // The page: a wrong secret is refused; the right one gets its program.
    let mut wrong = Request::builder()
        .uri(format!("/openscript/runner/host/{}/spec", id))
        .header("x-runner-token", "nope")
        .body(Body::empty())
        .unwrap();
    wrong
        .headers_mut()
        .insert("accept", "application/json".parse().unwrap());
    let (s, _) = t.h.json(wrong).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, spec) = t.page(Method::GET, &id, "spec", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(spec["program"].as_str().unwrap().contains("sha256:"));
    assert_eq!(spec["product"], "MIS");
    let (s, bars) = t.page(Method::GET, &id, "bars", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(bars["data"].as_array().unwrap().len(), 3);

    // An intent from a confirmed bar becomes a tagged order on the run's side.
    let v = t.buy(&id, 1, 2).await;
    assert_eq!(v["sent"], 1);
    {
        let placed = t.fake.placed.lock();
        let (mode, request, internal) = &placed[0];
        assert_eq!(*mode, RunMode::Sandbox);
        assert!(!internal);
        assert_eq!(request["strategy"], id);
        assert_eq!(request["symbol"], "SBIN");
        assert_eq!(request["quantity"], 2);
        assert_eq!(request["pricetype"], "MARKET");
        assert_eq!(request["product"], "MIS");
    }
    let (_, inbox) = t.page(Method::GET, &id, "inbox?after=0", None).await;
    let frame = &inbox["messages"][0]["frame"];
    assert_eq!(frame["intentId"], 1);
    assert_eq!(frame["status"], "working");
    assert_eq!(frame["orderRef"], "OID1");

    // The pump reads the fill back and tells the page.
    t.fake.set_status(
        RunMode::Sandbox,
        "OID1",
        json!({"order_status": "complete", "average_price": 100.0}),
    );
    t.h.ctx.trading.runner.pump_once(&id).await;
    let seq = inbox["messages"][0]["seq"].as_u64().unwrap();
    let (_, inbox) = t
        .page(Method::GET, &id, &format!("inbox?after={}", seq), None)
        .await;
    assert_eq!(inbox["messages"][0]["frame"]["status"], "filled");
    assert_eq!(inbox["messages"][0]["frame"]["filledQty"], 2);

    // An unconfirmed bar sends nothing; a refused shape is answered as one.
    let (s, _) = t
        .page(
            Method::POST,
            &id,
            "intents",
            Some(json!({"confirmed": false, "intents": []})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Books are this deployment's alone, recounted.
    let (s, ob) = t
        .call(
            Method::GET,
            &format!("/openscript/runner/orderbook/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let orders = ob["data"]["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0]["orderid"], "OID1");
    assert_eq!(ob["data"]["statistics"]["total_buy_orders"], 1);
    let (_, tb) = t
        .call(
            Method::GET,
            &format!("/openscript/runner/tradebook/{}", id),
            None,
        )
        .await;
    assert_eq!(tb["data"].as_array().unwrap().len(), 1);
    let (_, pb) = t
        .call(
            Method::GET,
            &format!("/openscript/runner/positions/{}", id),
            None,
        )
        .await;
    assert_eq!(
        pb["data"].as_array().unwrap().len(),
        1,
        "only contracts it traded"
    );
    assert_eq!(
        pb["total_unrealized_pnl"], 20.0,
        "its own fills, not the account's"
    );

    // A running deployment is not removed.
    let (s, v) = t
        .call(
            Method::DELETE,
            &format!("/openscript/runner/config/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"].as_str().unwrap().contains("is running"));

    // Pause: the page is closed, its channel answers no more, the position stays.
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/pause/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["file"], id);
    assert!(!t.host.is_open(&id));
    assert_eq!(t.h.ctx.trading.runner.task_count(), 0);
    let (s, _) = t.page(Method::GET, &id, "spec", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(t.fake.placed.lock().len(), 1, "a pause sends nothing");
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/pause/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["message"], format!("{} is not running.", id));

    // Stopped, its book is still read from the side it traded on.
    t.fake.analyzer.store(false, Ordering::SeqCst);
    let (_, ob) = t
        .call(
            Method::GET,
            &format!("/openscript/runner/orderbook/{}", id),
            None,
        )
        .await;
    assert_eq!(ob["data"]["orders"].as_array().unwrap().len(), 1);

    let (s, _) = t
        .call(
            Method::DELETE,
            &format!("/openscript/runner/config/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn close_squares_the_position_or_stays_running() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.call(
        Method::POST,
        &format!("/openscript/runner/start/{}", id),
        None,
    )
    .await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;

    // The exit is refused: the run stays open and managed.
    t.fake.refuse_orders.store(true, Ordering::SeqCst);
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/close/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"].as_str().unwrap().contains("still running"));
    assert!(t.h.ctx.trading.runner.is_running(&id));
    assert!(t.host.is_open(&id));

    // The exit fills: closed and stopped.
    t.fake.refuse_orders.store(false, Ordering::SeqCst);
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/close/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["message"], "t.oscript closed and stopped");
    let placed = t.fake.placed.lock().clone();
    let (mode, exit, internal) = placed.last().unwrap();
    assert_eq!(*mode, RunMode::Sandbox);
    assert!(internal, "the trader's own Stop");
    assert_eq!(exit["action"], "SELL");
    assert_eq!(exit["quantity"], 3);
    assert_eq!(exit["strategy"], id);
    assert!(!t.h.ctx.trading.runner.is_running(&id));
    assert_eq!(t.host.open_count(), 0);
}

/// LOG-01: a deployment keeps its id when it is started on the other side.
/// A sandbox position left by Pause (or logout, or quit) once made a later
/// live Stop sell it at the broker: a naked short of the sandbox quantity.
#[tokio::test]
async fn stop_after_switching_sides_closes_only_this_side() {
    for by_logout in [false, true] {
        let t = setup().await;
        let id = t.deploy("SBIN").await;
        t.leave_a_sandbox_position(&id, "buy", 3, by_logout).await;
        t.go_live(&id).await;
        let (s, v) = t.close(&id).await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        assert_eq!(v["message"], "t.oscript closed and stopped");
        // Nothing reached the broker: no exit, no cancel, for a sandbox fill.
        assert!(t.fake.placed_on(RunMode::Live).is_empty());
        assert!(t
            .fake
            .cancelled
            .lock()
            .iter()
            .all(|(m, _)| *m != RunMode::Live));
        // The sandbox position is still the sandbox's, untouched.
        assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1);
    }
}

#[tokio::test]
async fn stop_after_switching_sides_closes_the_live_fills_alone() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.leave_a_sandbox_position(&id, "buy", 3, false).await;
    t.go_live(&id).await;
    t.buy(&id, 1, 2).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let live = t.fake.placed_on(RunMode::Live);
    assert_eq!(live.len(), 2, "the live entry and one exit");
    assert_eq!(live[1]["action"], "SELL");
    assert_eq!(live[1]["quantity"], 2, "the live fills, never 2 + 3");
}

#[tokio::test]
async fn opposite_residuals_on_two_sides_do_not_cancel_out() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    // Sandbox short 3 and live long 3 netted to nothing, so Stop reported
    // "closed" and left the live long with nobody managing it.
    t.leave_a_sandbox_position(&id, "sell", 3, false).await;
    t.go_live(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let live = t.fake.placed_on(RunMode::Live);
    assert_eq!(live.len(), 2);
    assert_eq!(live[1]["action"], "SELL");
    assert_eq!(live[1]["quantity"], 3);
}

/// LOG-01, the cap: a Stop's exit is never larger than, nor opposite to,
/// what the destination itself holds in the contract.
#[tokio::test]
async fn stop_never_closes_more_than_the_destination_holds() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let runner = t.h.ctx.trading.runner.clone();
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    runner.pump_once(&id).await;

    // Closed outside the strategy (the end-of-day square-off): flat there,
    // so nothing is sent, which would have opened a short of 3.
    t.fake.hold_outside(RunMode::Sandbox, "SBIN", -3);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit sent");
    let log = std::fs::read_to_string(runner.logs_dir().join(&runner.logs_for(&id)[0])).unwrap();
    assert!(log.contains("holds no MIS SBIN position"), "{}", log);

    // The account holds less than the run's own fills: only that is closed.
    t.fake.hold_outside(RunMode::Sandbox, "SBIN", -1);
    t.start(&id).await;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["action"], "SELL");
    assert_eq!(sent[1]["quantity"], 2);

    // The account holds the other side: refused, nothing sent, still running.
    t.fake.hold_outside(RunMode::Sandbox, "SBIN", -4);
    t.start(&id).await;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("your account in sandbox mode is short 3"),
        "{}",
        v
    );
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2);
    assert!(runner.is_running(&id));

    // The position book cannot be read: refused, nothing sent, still running.
    t.fake.hold_outside(RunMode::Sandbox, "SBIN", 0);
    t.fake.unreadable_positions.store(true, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("could not be read just now"));
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2);
    assert!(runner.is_running(&id));
}

#[tokio::test]
async fn close_times_out_and_leaves_the_run_holding() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.call(
        Method::POST,
        &format!("/openscript/runner/start/{}", id),
        None,
    )
    .await;
    t.buy(&id, 1, 1).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    t.fake.fill_at_once.store(false, Ordering::SeqCst);
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/close/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("did not close its position in time"));
    assert!(t.h.ctx.trading.runner.is_running(&id));
    // And it trades on: the halt was lifted.
    let v = t.buy(&id, 2, 1).await;
    assert_eq!(v["sent"], 1);
}

#[tokio::test]
async fn live_runs_use_the_live_side_and_need_the_broker() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.analyzer.store(false, Ordering::SeqCst);
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"].as_str().unwrap().contains("broker"));
    assert_eq!(t.host.open_count(), 0);

    t.fake.broker.store(true, Ordering::SeqCst);
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(v["run"]["mode"], "live");
    // The toggle moving mid-run does not move the run's orders.
    t.fake.analyzer.store(true, Ordering::SeqCst);
    t.buy(&id, 1, 1).await;
    assert_eq!(t.fake.placed.lock()[0].0, RunMode::Live);
}

#[tokio::test]
async fn refusals_read_like_the_web() {
    let t = setup().await;
    let (s, v) = t
        .call(
            Method::POST,
            "/openscript/runner/start/missing.oscript",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        v["message"],
        "There is no strategy saved for missing.oscript."
    );
    let (s, _) = t
        .call(Method::POST, "/openscript/runner/start/bad%20name", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = t
        .call(
            Method::POST,
            "/openscript/runner/config/t.oscript",
            Some(json!({"symbol": "SBIN", "live": true})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"].as_str().unwrap().contains("also carried live"));
    // A write without the session's token never reaches the runner.
    let (s, _) =
        t.h.json(with(
            req(Method::POST, "/openscript/runner/start/t.oscript", None),
            &t.cookie,
            None,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = t.h.send(get("/openscript/runner/status")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // A source saved with no program cannot be started.
    t.call(
        Method::POST,
        "/openscript/d.oscript",
        Some(json!({"source": "draft"})),
    )
    .await;
    let (s, v) = t
        .call(Method::POST, "/openscript/runner/start/d.oscript", None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("no compiled program"));
    // No host in this copy of the app: refused with a sentence.
    let (s, v) = t
        .call(Method::GET, "/openscript/runner/instruments?q=S", None)
        .await;
    assert_eq!((s, v["data"].clone()), (StatusCode::OK, json!([])));
}

#[tokio::test]
async fn schedules_start_and_stop_on_the_ist_clock() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/schedule/{}", id),
            Some(json!({"start_time": "10:05", "stop_time": "10:30", "days": ["mon"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["schedule"]["days"], json!(["mon"]));
    assert_eq!(
        v["message"],
        format!("{} runs 10:05 to 10:30 IST on mon.", id)
    );
    let (_, st) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(st["scheduled"][0]["file"], id);

    let runner = t.h.ctx.trading.runner.clone();
    // 2026-10-05 is a Monday; the harness clock starts at 10:00 IST.
    runner.tick().await;
    assert!(!runner.is_running(&id));
    t.h.clock.set(ist(2026, 10, 5, 10, 5));
    runner.tick().await;
    assert!(runner.is_running(&id), "started at 10:05");
    t.h.clock.set(ist(2026, 10, 5, 10, 6));
    runner.tick().await;
    assert!(runner.is_running(&id));
    t.h.clock.set(ist(2026, 10, 5, 10, 30));
    runner.tick().await;
    assert!(!runner.is_running(&id), "stopped at 10:30");

    // A closed market and a signed-out app start nothing.
    t.h.clock.set(ist(2026, 10, 12, 10, 4));
    runner.tick().await;
    t.fake.closed_market.store(true, Ordering::SeqCst);
    t.h.clock.set(ist(2026, 10, 12, 10, 5));
    runner.tick().await;
    assert!(!runner.is_running(&id));
    t.fake.closed_market.store(false, Ordering::SeqCst);
    t.fake.nobody.store(true, Ordering::SeqCst);
    t.h.clock.set(ist(2026, 10, 19, 10, 4));
    runner.tick().await;
    t.h.clock.set(ist(2026, 10, 19, 10, 5));
    runner.tick().await;
    assert!(!runner.is_running(&id));

    let (s, v) = t
        .call(
            Method::DELETE,
            &format!("/openscript/runner/schedule/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], format!("{} is no longer scheduled.", id));
    let (s, _) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/schedule/{}", id),
            Some(json!({"start_time": "10:00", "stop_time": "09:00"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logout_pauses_every_run() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.call(
        Method::POST,
        &format!("/openscript/runner/start/{}", id),
        None,
    )
    .await;
    assert!(t.h.ctx.trading.runner.is_running(&id));
    let (s, _) = t.call(Method::POST, "/auth/logout", None).await;
    assert_eq!(s, StatusCode::OK);
    let runner = t.h.ctx.trading.runner.clone();
    for _ in 0..200 {
        if !runner.is_running(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!runner.is_running(&id));
    assert_eq!(t.host.open_count(), 0);
    assert_eq!(runner.task_count(), 0);
}

#[tokio::test]
async fn a_silent_page_is_noticed_and_its_run_ended() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let mut o = fast();
    o.order_poll = Duration::from_millis(20);
    o.heartbeat_timeout = Duration::from_millis(60);
    t.h.ctx.trading.runner.set_options(o);
    t.call(
        Method::POST,
        &format!("/openscript/runner/start/{}", id),
        None,
    )
    .await;
    let runner = t.h.ctx.trading.runner.clone();
    for _ in 0..200 {
        if !runner.is_running(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!runner.is_running(&id));
    assert!(!t.host.is_open(&id));
    let log = std::fs::read_to_string(runner.logs_dir().join(&runner.logs_for(&id)[0])).unwrap();
    assert!(log.contains("stopped answering"));
}

#[tokio::test]
async fn shutdown_leaves_no_task_or_page_and_refuses_new_starts() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let runner = t.h.ctx.trading.runner.clone();
    runner.start_scheduler();
    t.call(
        Method::POST,
        &format!("/openscript/runner/start/{}", id),
        None,
    )
    .await;
    assert_eq!(runner.task_count(), 2);
    assert_eq!(runner.open_pages(), 1);
    t.h.ctx.trading.shutdown().await;
    assert_eq!(runner.task_count(), 0);
    assert_eq!(runner.open_pages(), 0);
    assert!(!runner.is_running(&id));
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"].as_str().unwrap().contains("shutting down"));
}

#[tokio::test]
async fn many_starts_and_pauses_leave_nothing_behind() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let runner = t.h.ctx.trading.runner.clone();
    for _ in 0..100 {
        runner.start(&id).unwrap();
        runner.pause(&id).unwrap();
    }
    assert_eq!(runner.task_count(), 0);
    assert_eq!(t.host.open_count(), 0);
    assert_eq!(t.host.launches().len(), 100);
}

#[tokio::test]
async fn a_bracket_stops_the_run_and_sends_nothing() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.call(
        Method::POST,
        &format!("/openscript/runner/start/{}", id),
        None,
    )
    .await;
    let (s, v) = t
        .page(
            Method::POST,
            &id,
            "intents",
            Some(json!({"confirmed": true, "intents": [{"intentId": 1, "kind": "bracket", "tag": "x"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["stopped"], true);
    assert!(t.fake.placed.lock().is_empty());
    assert!(!t.h.ctx.trading.runner.is_running(&id));
}
