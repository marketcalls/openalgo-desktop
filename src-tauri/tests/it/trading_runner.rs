//! The OpenScript live runner with a fake host and fake services: lifecycle
//! (start, pause, close), status shapes, orders tagged and booked per
//! deployment, sandbox versus live, schedules on the injected IST clock,
//! teardown on logout and shutdown, and no task or page left behind.

use crate::webui_support::{get, ist, req, with, H};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::NaiveDate;
use openalgo_desktop_lib::services::order_reconciler::{
    BookSnapshot, EntryGate, OrderOwner, ENTRIES_PAUSED,
};
use openalgo_desktop_lib::strategy::dispatch::{
    Book, BookOrder, DispatchResult, OrderStatusResult, RunMode,
};
use openalgo_desktop_lib::trading::runner::host::{fragment_of, RecordingHost, RunnerHost};
use openalgo_desktop_lib::trading::runner::reconcile::RunnerOrders;
use openalgo_desktop_lib::trading::runner::services::{Bar, RunnerServices};
use openalgo_desktop_lib::trading::runner::RunnerOptions;
use openalgo_desktop_lib::trading::scripts::source_hash;
use openalgo_desktop_lib::trading::Trading;
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
    unreadable_mode: AtomicBool,
    lagging_positions: AtomicBool,
    next: AtomicU64,
    placed: Mutex<Vec<(RunMode, Value, bool)>>,
    orders: Mutex<Vec<FakeOrder>>,
    cancelled: Mutex<Vec<(RunMode, String)>>,
    /// Size held outside any strategy, per side and symbol (NSE, MIS),
    /// added to what the position book reports.
    outside: Mutex<Vec<(RunMode, String, i64)>>,
    /// What the destination does with a cancel. A cancel is a request: the
    /// order may stay open, or fill partly before it ends.
    cancel_mode: Mutex<CancelMode>,
    /// Placements whose answer is lost after the order reached the
    /// destination (LOG-08): it holds the order, the runner hears nothing
    /// definite.
    lose_reply: AtomicU64,
    /// Placements with no definite answer that never reached it.
    never_arrives: AtomicU64,
    /// Who decides whether new entries wait (the app's order reconciler in
    /// the tests that set it).
    gate: Mutex<Option<Arc<dyn EntryGate>>>,
    /// Order status reads that never answer (TIME-03).
    status_hangs: AtomicBool,
}

/// A placement the destination neither accepted nor refused.
fn uncertain() -> DispatchResult {
    DispatchResult {
        ok: false,
        uncertain: true,
        error: Some(
            "The broker did not confirm this order. Check the order book before placing it again."
                .into(),
        ),
        ..Default::default()
    }
}

/// Take one from a countdown, if any is left.
fn take(n: &AtomicU64) -> bool {
    n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
        .is_ok()
}

#[derive(Default, Clone, Copy)]
enum CancelMode {
    /// Cancelled at once, nothing more filled.
    #[default]
    Accept,
    /// Refused (a rate limit, the network).
    Refuse,
    /// Accepted, but the order stays open.
    StayOpen,
    /// Accepted; the order ends cancelled after this many filled.
    FilledThenCancelled(i64),
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

    /// One read of a side's order book, as the order reconciler hands it to
    /// its owners. Each row carries the time of the read as its own (IST),
    /// so the matcher can tell it was placed after the order it looks for.
    fn snapshot(
        &self,
        mode: RunMode,
        read_at: chrono::DateTime<chrono::Utc>,
        owners: Vec<Arc<dyn OrderOwner>>,
    ) -> BookSnapshot {
        let at = read_at
            .with_timezone(&chrono_tz::Asia::Kolkata)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let rows = self
            .orders
            .lock()
            .iter()
            .filter(|o| o.mode == mode)
            .filter_map(|o| {
                let s = &o.status;
                let filled = if s["order_status"] == "complete" {
                    s["quantity"].clone()
                } else {
                    s.get("filled_quantity").cloned().unwrap_or(json!(0))
                };
                BookOrder::from_row(&json!({
                    "orderid": o.id, "symbol": s["symbol"], "exchange": s["exchange"],
                    "action": s["action"], "quantity": s["quantity"], "product": s["product"],
                    "pricetype": s["pricetype"], "order_status": s["order_status"],
                    "filled_quantity": filled, "average_price": s["average_price"],
                    "timestamp": at,
                }))
            })
            .collect();
        let mut book = BookSnapshot::new(rows, read_at);
        book.owners = owners;
        book
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
                *net.entry((symbol.clone(), "NSE".into(), "MIS".into()))
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
    fn analyzer_on(&self) -> Result<bool, String> {
        if self.unreadable_mode.load(Ordering::SeqCst) {
            return Err("database is locked".into());
        }
        Ok(self.analyzer.load(Ordering::SeqCst))
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
        if take(&self.never_arrives) {
            return uncertain();
        }
        let id = format!("OID{}", self.next.fetch_add(1, Ordering::SeqCst) + 1);
        let mut status = json!({"order_status": "open", "quantity": request["quantity"],
            "action": request["action"], "symbol": request["symbol"],
            "exchange": request["exchange"], "product": request["product"],
            "pricetype": request["pricetype"]});
        if self.fill_at_once.load(Ordering::SeqCst) {
            status["order_status"] = json!("complete");
            status["average_price"] = json!(100.0);
        }
        self.orders.lock().push(FakeOrder {
            mode,
            id: id.clone(),
            status,
        });
        if take(&self.lose_reply) {
            // The destination holds the order; its answer never arrives.
            return uncertain();
        }
        DispatchResult {
            ok: true,
            broker_order_id: Some(id.clone()),
            response: json!({"status": "success", "orderid": id}),
            ..Default::default()
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
        match *self.cancel_mode.lock() {
            CancelMode::Accept => o.status["order_status"] = json!("cancelled"),
            CancelMode::Refuse => return DispatchResult::refused("Too many requests"),
            CancelMode::StayOpen => {}
            CancelMode::FilledThenCancelled(n) => {
                o.status["order_status"] = json!("cancelled");
                o.status["filled_quantity"] = json!(n);
                o.status["average_price"] = json!(100.0);
            }
        }
        DispatchResult {
            ok: true,
            broker_order_id: Some(orderid.into()),
            ..Default::default()
        }
    }
    async fn order_status(&self, mode: RunMode, orderid: &str) -> OrderStatusResult {
        if self.status_hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
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
                if self.lagging_positions.load(Ordering::SeqCst) {
                    // A successful answer that has not caught up with a fill.
                    return Ok(json!({"status": "success", "data": []}));
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
    fn entry_refusal(&self, mode: RunMode) -> Option<String> {
        self.gate
            .lock()
            .as_ref()
            .and_then(|g| g.entry_refusal(mode))
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

    /// Quit and reopen the runner on the same database, as an app restart
    /// does: every run is paused on the way out, and only what the database
    /// holds carries over. The reopened runner is driven directly.
    async fn restart(&self) -> Trading {
        self.h.ctx.trading.runner.shutdown().await;
        let trading = Trading::new(
            self.h.dir.path(),
            self.h.ctx.sqlite.clone(),
            self.h.clock.clone(),
            self.fake.clone(),
        );
        trading.runner.set_host(self.host.clone());
        trading.runner.set_options(fast());
        trading
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
        // The trader also holds 10 SBIN at the broker of their own, so the
        // cap cannot hide a ledger that mixed the sides: only the side
        // filter keeps the sandbox 3 out of this Stop.
        t.fake.hold_outside(RunMode::Live, "SBIN", 10);
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
    // 10 of the trader's own at the broker: the cap does not shrink a mixed
    // ledger of 5 back to 2, so only the side filter passes this.
    t.fake.hold_outside(RunMode::Live, "SBIN", 10);
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

/// The page channel's answer as the page reads it, with `seq` renumbered
/// from 1 so it can be compared with a recorded answer.
fn on_the_wire(answer: &Value) -> Value {
    let mut a = answer.clone();
    if let Some(messages) = a["messages"].as_array_mut() {
        for (n, m) in messages.iter_mut().enumerate() {
            m["seq"] = json!(n as u64 + 1);
        }
    }
    a
}

/// A recorded page-channel answer, shared with the runner page's own test
/// (`src/runner/handshake.test.ts`), which replays it through the real
/// transport and Driver. What Rust answers and what the page acts on are
/// held to the same file.
fn page_fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/openscript")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

impl T {
    /// The highest `seq` the page has been answered so far.
    async fn inbox_seen(&self, run: &str) -> u64 {
        let (_, inbox) = self.page(Method::GET, run, "inbox?after=0", None).await;
        inbox["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["seq"].as_u64())
            .max()
            .unwrap_or(0)
    }
}

/// LOG-02: Stop halts the page before it closes. A close that did not
/// happen and left nothing working resumes the page, which the page needs to
/// send its script's own exits again. Before, only Rust lifted its own halt,
/// and the page refused every later order for good.
#[tokio::test]
async fn a_refused_close_resumes_the_page() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    let seen = t.inbox_seen(&id).await;

    t.fake.refuse_orders.store(true, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"].as_str().unwrap().contains("still running"));
    let (_, inbox) = t
        .page(Method::GET, &id, &format!("inbox?after={}", seen), None)
        .await;
    assert_eq!(
        on_the_wire(&inbox),
        page_fixture("inbox_after_refused_stop.json")
    );
    let (_, st) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(st["running"][0]["state"], "running");

    // The script's own exit goes out again.
    t.fake.refuse_orders.store(false, Ordering::SeqCst);
    let v = t.order(&id, 2, "sell", 3).await;
    assert_eq!(v["sent"], 1);
}

/// LOG-02 (replaces `close_times_out_and_leaves_the_run_holding`, which
/// proved only that Rust lifted its own halt): an exit that has not filled
/// may still fill, so the run is close pending, not resumed. It sends
/// nothing, says so in its state, and a second Stop after the exit filled
/// sends no second exit.
#[tokio::test]
async fn an_unfilled_exit_leaves_the_close_pending_and_halted() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 1).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    let seen = t.inbox_seen(&id).await;
    t.fake.fill_at_once.store(false, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let message = v["message"].as_str().unwrap();
    assert!(message.contains("has not filled yet"), "{}", message);
    assert!(message.contains("close is pending"), "{}", message);
    assert!(!message.contains("still running and still holding"));
    assert!(t.h.ctx.trading.runner.is_running(&id));
    let (_, st) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(st["running"][0]["state"], "close_pending");

    // No resume reaches the page, and nothing new is sent.
    let (_, inbox) = t
        .page(Method::GET, &id, &format!("inbox?after={}", seen), None)
        .await;
    assert_eq!(
        on_the_wire(&inbox),
        page_fixture("inbox_after_pending_stop.json")
    );
    let v = t.buy(&id, 2, 1).await;
    assert_eq!(v["sent"], 0);
    assert_eq!(v["halted"], true);

    // The exit fills; Stop again counts it and sends no second exit.
    t.fake.set_status(
        RunMode::Sandbox,
        "OID2",
        json!({"order_status": "complete", "average_price": 100.0}),
    );
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2);
    assert!(!t.h.ctx.trading.runner.is_running(&id));
}

/// LOG-03: a cancel is a request, not proof. Stop waits until the
/// destination says each working order of the run is finished, and measures
/// the position only then.
#[tokio::test]
async fn a_stop_waits_for_working_orders_to_finish() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.start(&id).await;
    t.buy(&id, 1, 10).await; // working, nothing filled

    // The cancel is refused: the order may still fill, so nothing is closed.
    *t.fake.cancel_mode.lock() = CancelMode::Refuse;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("Order OID1 of this strategy is still working"));
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit");
    assert!(t.h.ctx.trading.runner.is_running(&id));

    // Accepted, but the order is still open at the deadline: the same.
    *t.fake.cancel_mode.lock() = CancelMode::StayOpen;
    let (s, _) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit");
    assert!(t.h.ctx.trading.runner.is_running(&id));

    // Cancelled after 4 of 10 filled: exactly those 4 are closed.
    *t.fake.cancel_mode.lock() = CancelMode::FilledThenCancelled(4);
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["action"], "SELL");
    assert_eq!(sent[1]["quantity"], 4);
}

/// TIME-03: the close budget bounds the whole close. Each open order's
/// status was read one after another before the deadline was ever checked,
/// so three reads that never answer held Stop forever. Now no read outlasts
/// the deadline: Stop answers within it, sends no exit, and says the orders
/// are still working, with the position still held.
#[tokio::test]
async fn a_stop_answers_within_its_budget_when_status_reads_hang() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.start(&id).await;
    for intent in 1..=3 {
        t.buy(&id, intent, 1).await; // working, nothing filled
    }
    *t.fake.cancel_mode.lock() = CancelMode::StayOpen;
    t.fake.status_hangs.store(true, Ordering::SeqCst);
    let closed = tokio::time::timeout(Duration::from_secs(10), t.close(&id)).await;
    let (s, v) = closed.expect("Stop must answer within its close budget");
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    let message = v["message"].as_str().unwrap();
    assert!(
        message.contains("Orders OID1, OID2, OID3 of this strategy are still working"),
        "{}",
        message
    );
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 3, "no exit");
    assert!(t.h.ctx.trading.runner.is_running(&id));
}

/// LOG-04b: the cap reads the destination's position book, and a book that
/// has not caught up with a fill answers successfully with no row for the
/// contract. Read as zero, an owned BUY 3 was treated as already flat and
/// the run torn down as closed. Unknown is not flat: nothing is sent, the run
/// stays running and managed, and Stop works once the book shows it.
#[tokio::test]
async fn a_lagging_empty_position_book_is_not_flat() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;

    t.fake.lagging_positions.store(true, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    let message = v["message"].as_str().unwrap();
    assert!(message.contains("do not show SBIN yet"), "{}", message);
    assert!(message.contains("still running"), "{}", message);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit");
    assert!(t.h.ctx.trading.runner.is_running(&id));
    let (_, st) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(st["running"][0]["state"], "running", "managed, not halted");

    // The book catches up: the same Stop closes exactly the 3.
    t.fake.lagging_positions.store(false, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["action"], "SELL");
    assert_eq!(sent[1]["quantity"], 3);
}

/// LOG-04: the broker's answer to a placement is what lets the run follow
/// the order. Its write was ignored on failure, so the row kept no order id,
/// was never polled, its fill never counted, and Stop reported closed while
/// the account held it. Now the answer is kept on the run until it is saved:
/// nothing new is sent, Stop does not report closed, the run says so, and
/// once it is saved the fill is counted and Stop closes it.
#[tokio::test]
async fn an_answer_that_cannot_be_saved_is_kept_until_it_is() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let runner = t.h.ctx.trading.runner.clone();
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    let sql = |s: &str| t.h.ctx.sqlite.conn().unwrap().execute_batch(s).unwrap();
    sql(
        "CREATE TRIGGER no_ack BEFORE UPDATE OF orderid ON openscript_orders
         BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
    );
    let v = t.buy(&id, 1, 3).await;
    assert_eq!(v["sent"], 1, "the order went out");
    runner.pump_once(&id).await;
    let (_, st) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(st["running"][0]["unsaved_orders"], 1);

    // Nothing new is sent while the answer is unsaved.
    let v = t.buy(&id, 2, 1).await;
    assert_eq!(v["sent"], 0);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1);
    // Stop does not report closed: the order may have filled.
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("has not been saved yet"));
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit");
    assert!(runner.is_running(&id));

    // Saved at last: the fill is counted and Stop closes exactly it.
    sql("DROP TRIGGER no_ack;");
    runner.pump_once(&id).await;
    let (_, st) = t.call(Method::GET, "/openscript/runner/status", None).await;
    assert_eq!(st["running"][0]["unsaved_orders"], 0);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["action"], "SELL");
    assert_eq!(sent[1]["quantity"], 3);
}

/// LOG-04: a ledger that cannot be read is not a flat one.
#[tokio::test]
async fn a_stop_that_cannot_read_its_orders_closes_nothing() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    let rename = |from: &str, to: &str| {
        t.h.ctx
            .sqlite
            .conn()
            .unwrap()
            .execute_batch(&format!("ALTER TABLE {} RENAME TO {}", from, to))
            .unwrap();
    };
    rename("openscript_orders", "openscript_orders_unreadable");
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"].as_str().unwrap().contains("could not be read"));
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit");
    assert!(t.h.ctx.trading.runner.is_running(&id));
    // Its books say so too, instead of an empty book.
    let (s, _) = t
        .call(
            Method::GET,
            &format!("/openscript/runner/positions/{}", id),
            None,
        )
        .await;
    assert!(!s.is_success());

    rename("openscript_orders_unreadable", "openscript_orders");
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["quantity"], 3);
}

/// DB-04: the prune runs when the app starts. It deleted every row older
/// than 180 days, so a position held that long vanished from the ledger and
/// Stop had nothing to close. Reopened on the same database, the old
/// position survives the start-up prune and Stop closes it.
#[tokio::test]
async fn an_old_position_survives_the_prune_and_stop_closes_it() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    let today = t.h.ctx.now();
    t.h.clock.set(today - chrono::Duration::days(200));
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;
    t.h.clock.set(today);

    // The app restarts 200 days later: its start prunes old rows.
    let trading = t.restart().await;
    trading.start(today, &t.h.ctx.sqlite);
    let runner = trading.runner.clone();
    runner.start(&id).unwrap();
    runner.close(&id).await.unwrap();
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["action"], "SELL");
    assert_eq!(sent[1]["quantity"], 3);
    trading.shutdown().await;
}

/// LOG-01 across a restart: the side of every row is in the database, so a
/// sandbox position left by quitting the app is never closed at the broker
/// by a live Stop after it reopens.
#[tokio::test]
async fn a_sandbox_position_left_by_a_restart_is_never_closed_live() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;

    let trading = t.restart().await;
    let runner = trading.runner.clone();
    t.fake.analyzer.store(false, Ordering::SeqCst);
    t.fake.broker.store(true, Ordering::SeqCst);
    t.fake.hold_outside(RunMode::Live, "SBIN", 10);
    assert_eq!(runner.start(&id).unwrap().mode, RunMode::Live);
    runner.close(&id).await.unwrap();
    assert!(t.fake.placed_on(RunMode::Live).is_empty());
    trading.shutdown().await;
}

/// LOG-02 and LOG-03 across a restart: a close left pending (its exit still
/// open) is in the ledger. If the exit fills while the app is closed, the
/// next Stop counts that fill and sends no second exit; if it is still open,
/// the next Stop cancels it before measuring, so two exits never work at once.
#[tokio::test]
async fn a_pending_close_survives_a_restart_without_a_second_exit() {
    for fills_while_closed in [true, false] {
        let t = setup().await;
        let id = t.deploy("SBIN").await;
        t.fake.fill_at_once.store(true, Ordering::SeqCst);
        t.start(&id).await;
        t.buy(&id, 1, 1).await;
        t.h.ctx.trading.runner.pump_once(&id).await;
        t.fake.fill_at_once.store(false, Ordering::SeqCst);
        let (s, _) = t.close(&id).await;
        assert_eq!(s, StatusCode::CONFLICT, "the exit OID2 is still open");

        let trading = t.restart().await;
        let runner = trading.runner.clone();
        if fills_while_closed {
            t.fake.set_status(
                RunMode::Sandbox,
                "OID2",
                json!({"order_status": "complete", "average_price": 100.0}),
            );
        }
        t.fake.fill_at_once.store(true, Ordering::SeqCst);
        runner.start(&id).unwrap();
        runner.close(&id).await.unwrap();
        let sent = t.fake.placed_on(RunMode::Sandbox);
        if fills_while_closed {
            assert_eq!(sent.len(), 2, "no second exit");
        } else {
            assert_eq!(sent.len(), 3, "the open exit was replaced, not doubled");
            assert!(t
                .fake
                .cancelled
                .lock()
                .contains(&(RunMode::Sandbox, "OID2".to_string())));
            assert_eq!(sent[2]["quantity"], 1);
        }
        trading.shutdown().await;
    }
}

/// LOG-04 across a restart: the answer kept on the run is lost when the app
/// stops, and the row is left open with no order number. The ledger still
/// shows it, so after the restart the run says it cannot follow that order
/// and Stop does not report closed (before, it closed nothing and reported
/// success while the broker may hold the fill).
#[tokio::test]
async fn an_order_with_no_number_blocks_stop_after_a_restart() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    let sql = |s: &str| t.h.ctx.sqlite.conn().unwrap().execute_batch(s).unwrap();
    sql(
        "CREATE TRIGGER no_ack BEFORE UPDATE OF orderid ON openscript_orders
         BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
    );
    t.buy(&id, 1, 3).await;

    let trading = t.restart().await;
    sql("DROP TRIGGER no_ack;");
    let runner = trading.runner.clone();
    runner.start(&id).unwrap();
    runner.pump_once(&id).await;
    assert_eq!(runner.status_of(&id).unwrap().unresolved_orders, 1);
    let refused = runner.close(&id).await.unwrap_err();
    assert_eq!(refused.status, 409);
    assert!(
        refused.message.contains("has no order number"),
        "{}",
        refused.message
    );
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1, "no exit");
    assert!(runner.is_running(&id));
    assert!(runner.status_of(&id).unwrap().close_pending);
    trading.shutdown().await;
}

/// LOG-09: a start decides its side from analyzer mode. Unreadable, it was
/// taken as off, so a deployment meant for the sandbox started live while a
/// broker was connected.
#[tokio::test]
async fn a_start_refuses_when_analyzer_mode_cannot_be_read() {
    let t = setup().await;
    let id = t.deploy("SBIN").await;
    t.fake.broker.store(true, Ordering::SeqCst);
    t.fake.unreadable_mode.store(true, Ordering::SeqCst);
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("could not read whether analyzer mode is on"));
    assert!(!t.h.ctx.trading.runner.is_running(&id));
    assert_eq!(t.host.open_count(), 0);
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

/// Broker switching, row 1 (LOG-07): a running Live OpenScript run refuses
/// a switch away from the logged-in broker, named, until it is paused; a
/// Sandbox run never does. The real guard, through the Broker page's save.
#[tokio::test]
async fn a_live_openscript_run_refuses_a_broker_switch_until_it_is_paused() {
    use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
    use openalgo_desktop_lib::state::BrokerSession;
    let t = setup().await;
    let ctx = &t.h.ctx;
    let session = || BrokerSession {
        broker_id: "zerodha".into(),
        auth_token: "mock-access-token".into(),
        feed_token: None,
        user_id: "AB1234".into(),
        user_name: None,
        authenticated_at: ctx.now(),
    };
    BrokerAuthService::persist(ctx, &session()).unwrap();
    let switch = || {
        t.call(
            Method::POST,
            "/api/broker/credentials",
            Some(json!({"broker": "angel"})),
        )
    };
    // A Sandbox run does not count.
    let id = t.deploy("SBIN").await;
    assert_eq!(t.start(&id).await["run"]["mode"], "sandbox");
    assert!(
        openalgo_desktop_lib::services::broker_switch::refusal(ctx, "zerodha").is_none(),
        "a Sandbox run does not block"
    );
    t.h.ctx.trading.runner.pause(&id).unwrap();
    t.go_live(&id).await;
    let (s, v) = switch().await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    let why = v["message"].as_str().unwrap();
    assert!(
        why.starts_with("Zerodha still has 1 running Live OpenScript run."),
        "{}",
        why
    );
    assert!(
        why.contains("stop the strategies and protection"),
        "{}",
        why
    );
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "zerodha");
    t.h.ctx.trading.runner.pause(&id).unwrap();
    let (s, v) = switch().await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["broker_switched"], true, "{}", v);
    assert!(ctx.get_broker_session().is_none());
}

// ------------------------------------------------------------ LOG-08

impl T {
    /// The runner, as the order reconciler's owner of its orders.
    fn owner(
        &self,
        runner: &Arc<openalgo_desktop_lib::trading::runner::Runner>,
    ) -> Arc<dyn OrderOwner> {
        Arc::new(RunnerOrders(Arc::downgrade(runner)))
    }

    fn row_status(&self, id: i64) -> (String, Option<String>, i64) {
        self.h
            .ctx
            .sqlite
            .conn()
            .unwrap()
            .query_row(
                "SELECT status, orderid, filled_quantity FROM openscript_orders WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    fn row_message(&self, id: i64) -> Option<String> {
        self.h
            .ctx
            .sqlite
            .conn()
            .unwrap()
            .query_row(
                "SELECT message FROM openscript_orders WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Fold `reads` reads of a side's book, 10 s apart from `from`, as the
    /// reconciler would. Answers how many rows stay unresolved.
    async fn reads(
        &self,
        owner: &Arc<dyn OrderOwner>,
        mode: RunMode,
        from: chrono::DateTime<chrono::Utc>,
        reads: i64,
    ) -> usize {
        let mut left = 0;
        for n in 0..reads {
            let at = from + chrono::Duration::seconds(10 * n);
            left = owner
                .reconcile(mode, &self.fake.snapshot(mode, at, vec![owner.clone()]))
                .await;
        }
        left
    }

    /// The frames the page has received for one intent, oldest first.
    async fn frames(&self, run: &str, intent: i64) -> Vec<Value> {
        let (_, inbox) = self.page(Method::GET, run, "inbox?after=0", None).await;
        inbox["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "frame" && m["frame"]["intentId"] == intent)
            .map(|m| m["frame"].clone())
            .collect()
    }
}

/// LOG-08: Stop's exit reaches the destination and its answer is lost. It
/// was treated as refused ("could not be closed, try again"), so each Stop
/// pressed again measured the same filled rows and sent another SELL. Now
/// the exit is unconfirmed: no further Stop, before or after a restart, sends
/// a second one, and once the order book shows it the run closes with exactly
/// one SELL.
#[tokio::test]
async fn a_lost_exit_reply_sends_one_sell_across_repeated_stops_and_a_restart() {
    let t = setup().await;
    // Book reads are driven by hand.
    t.h.ctx.reconciler.stop();
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;

    t.fake.lose_reply.store(1, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"].as_str().unwrap().contains("did not confirm"));
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2);
    for _ in 0..3 {
        let (s, v) = t.close(&id).await;
        assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    }
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2, "one SELL only");
    assert_eq!(t.row_status(2).0, "unconfirmed");

    // The app restarts: the row is still unconfirmed, and Stop still waits.
    let trading = t.restart().await;
    let runner = trading.runner.clone();
    runner.start(&id).unwrap();
    assert_eq!(runner.close(&id).await.unwrap_err().status, 409);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2, "one SELL only");

    // The order book settles it: bound to OID2, filled; Stop then closes.
    let owner = t.owner(&runner);
    let book = t
        .fake
        .snapshot(RunMode::Sandbox, t.h.ctx.now(), vec![owner.clone()]);
    assert_eq!(owner.reconcile(RunMode::Sandbox, &book).await, 0);
    assert_eq!(
        t.row_status(2),
        ("filled".to_string(), Some("OID2".to_string()), 3)
    );
    runner.close(&id).await.unwrap();
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2, "exactly one SELL in all");
    assert_eq!(sent[1]["action"], "SELL");
    trading.shutdown().await;
}

/// LOG-08: an entry whose answer is lost was recorded rejected and a
/// rejected frame told the script it had no order, so it could enter again
/// while the first one filled. Now the intent stays pending (`placed`), the
/// run sends nothing new, and the book settles it into a fill the script
/// sees and Stop closes.
#[tokio::test]
async fn a_lost_entry_reply_keeps_the_intent_pending_and_blocks_new_orders() {
    let t = setup().await;
    t.h.ctx.reconciler.stop();
    let id = t.deploy("SBIN").await;
    let runner = t.h.ctx.trading.runner.clone();
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.fake.lose_reply.store(1, Ordering::SeqCst);
    let v = t.buy(&id, 1, 3).await;
    assert_eq!(v["sent"], 0);
    let frames = t.frames(&id, 1).await;
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["status"], "placed", "pending, never rejected");

    // Nothing new goes out, and Stop does not go ahead.
    let v = t.buy(&id, 2, 1).await;
    assert_eq!(v["unconfirmed"], true);
    let (s, _) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 1);

    // Settled from the book: the script hears the fill.
    let owner = t.owner(&runner);
    let book = t
        .fake
        .snapshot(RunMode::Sandbox, t.h.ctx.now(), vec![owner.clone()]);
    assert_eq!(owner.reconcile(RunMode::Sandbox, &book).await, 0);
    let frames = t.frames(&id, 1).await;
    let last = frames.last().unwrap();
    assert_eq!(last["status"], "filled");
    assert_eq!(last["filledQty"], 3);
    assert_eq!(last["orderRef"], "OID1");
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["quantity"], 3);
}

/// LOG-08 P0-3: absence. A sandbox order missing from the sandbox's book
/// over repeated reads was not placed: the row is rejected and the run
/// trades again. A live order missing from the broker's book is never
/// released on absence alone, however long (the book can lag; a wrong
/// release sends a second order): it is held, listed for the trader on the
/// Health page, and released only when the trader marks it not placed.
#[tokio::test]
async fn absence_releases_a_sandbox_row_and_a_live_one_only_on_the_traders_word() {
    for mode in [RunMode::Sandbox, RunMode::Live] {
        let t = setup().await;
        t.h.ctx.reconciler.stop();
        let id = t.deploy("SBIN").await;
        let runner = t.h.ctx.trading.runner.clone();
        if mode == RunMode::Live {
            t.fake.analyzer.store(false, Ordering::SeqCst);
            t.fake.broker.store(true, Ordering::SeqCst);
        }
        t.start(&id).await;
        t.fake.never_arrives.store(1, Ordering::SeqCst);
        t.buy(&id, 1, 3).await;
        let owner = t.owner(&runner);
        let t0 = t.h.ctx.now();
        let left = t.reads(&owner, mode, t0, 6).await;
        let v = t.buy(&id, 2, 1).await;
        if mode == RunMode::Sandbox {
            assert_eq!(left, 0);
            assert_eq!(t.row_status(1).0, "rejected");
            assert_eq!(v["sent"], 1, "the run trades again");
            assert!(t.h.ctx.reconciler.held_orders().is_empty());
            continue;
        }
        assert_eq!(left, 1, "kept unconfirmed");
        assert_eq!(t.row_status(1).0, "unconfirmed");
        assert_eq!(v["sent"], 0, "still nothing new");
        // Held for the trader, through the owner the app registered.
        let held = t.h.ctx.reconciler.held_orders();
        assert_eq!(held.len(), 1, "{:?}", held);
        assert_eq!(held[0].owner, "openscript");
        assert_eq!(held[0].id, "1");
        assert!(held[0].message.contains("OpenScript t.oscript"));
        assert!(held[0].message.contains("mark it as not placed"));
        // Long after the release window: still held, never released alone.
        let late = t0
            + chrono::Duration::seconds(
                openalgo_desktop_lib::services::order_reconciler::LIVE_RELEASE_SECS + 60,
            );
        assert_eq!(t.reads(&owner, mode, late, 2).await, 1);
        assert_eq!(t.row_status(1).0, "unconfirmed");
        // The trader marks it not placed: the run trades again.
        assert_eq!(owner.confirm_not_placed("1").await, Ok(true));
        assert_eq!(t.row_status(1).0, "rejected");
        assert_eq!(
            t.row_message(1).as_deref(),
            Some(openalgo_desktop_lib::trading::runner::reconcile::MARKED_NOT_PLACED)
        );
        let frames = t.frames(&id, 1).await;
        assert_eq!(frames.last().unwrap()["status"], "rejected");
        assert!(t.h.ctx.reconciler.held_orders().is_empty());
        assert_eq!(owner.confirm_not_placed("1").await, Ok(false), "once");
        let v = t.buy(&id, 3, 1).await;
        assert_eq!(v["sent"], 1, "the run trades again");
    }
}

/// LOG-08 P0-3 with Stop: a live exit that never reached the broker. Stop
/// pressed again sends nothing while it is unconfirmed, and nothing releases
/// it on absence alone; once the trader marks it not placed, Stop closes the
/// position with one SELL.
#[tokio::test]
async fn a_held_live_exit_is_sent_again_only_after_the_trader_marks_it() {
    let t = setup().await;
    t.h.ctx.reconciler.stop();
    let id = t.deploy("SBIN").await;
    let runner = t.h.ctx.trading.runner.clone();
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.go_live(&id).await;
    t.buy(&id, 1, 3).await;
    runner.pump_once(&id).await;
    t.fake.never_arrives.store(1, Ordering::SeqCst);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(v["message"].as_str().unwrap().contains("did not confirm"));
    let owner = t.owner(&runner);
    assert_eq!(t.reads(&owner, RunMode::Live, t.h.ctx.now(), 6).await, 1);
    for _ in 0..2 {
        let (s, _) = t.close(&id).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }
    assert_eq!(t.fake.placed_on(RunMode::Live).len(), 2, "one SELL tried");
    let held = t.h.ctx.reconciler.held_orders();
    assert_eq!(held.len(), 1, "{:?}", held);
    assert_eq!((held[0].action.as_str(), held[0].quantity), ("SELL", 3));

    assert_eq!(owner.confirm_not_placed(&held[0].id).await, Ok(true));
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let live = t.fake.placed_on(RunMode::Live);
    assert_eq!(live.len(), 3);
    assert_eq!(live[2]["action"], "SELL");
    assert_eq!(live[2]["quantity"], 3);
}

/// LOG-08 across a restart: the app stopped after an entry reached the
/// destination and before its answer was recorded, so its row has no order
/// number. It blocked Stop for good. Now the app's start marks it
/// unconfirmed, the order book settles it, and Stop closes with one SELL.
#[tokio::test]
async fn an_order_left_without_its_answer_is_settled_from_the_book_after_a_restart() {
    let t = setup().await;
    t.h.ctx.reconciler.stop();
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    let sql = |s: &str| t.h.ctx.sqlite.conn().unwrap().execute_batch(s).unwrap();
    sql(
        "CREATE TRIGGER no_ack BEFORE UPDATE OF orderid ON openscript_orders
         BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
    );
    t.buy(&id, 1, 3).await;
    let trading = t.restart().await;
    sql("DROP TRIGGER no_ack;");
    assert_eq!(t.row_status(1), ("placed".to_string(), None, 0));
    trading.start(t.h.ctx.now(), &t.h.ctx.sqlite);
    assert_eq!(t.row_status(1).0, "unconfirmed");

    let runner = trading.runner.clone();
    runner.start(&id).unwrap();
    let owner = t.owner(&runner);
    let book = t
        .fake
        .snapshot(RunMode::Sandbox, t.h.ctx.now(), vec![owner.clone()]);
    assert_eq!(owner.reconcile(RunMode::Sandbox, &book).await, 0);
    assert_eq!(
        t.row_status(1),
        ("filled".to_string(), Some("OID1".to_string()), 3)
    );
    runner.close(&id).await.unwrap();
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        (sent[1]["action"].clone(), sent[1]["quantity"].clone()),
        (json!("SELL"), json!(3))
    );
    trading.shutdown().await;
}

/// LOG-08, the whole app: the runner is registered with the order
/// reconciler, whose pass reads the real sandbox book while the runner holds
/// an unconfirmed sandbox row, and settles it there.
#[tokio::test]
async fn the_reconciler_settles_the_runners_unconfirmed_rows() {
    let t = setup().await;
    t.h.ctx.reconciler.stop();
    let id = t.deploy("SBIN").await;
    t.start(&id).await;
    t.fake.never_arrives.store(1, Ordering::SeqCst);
    t.buy(&id, 1, 3).await;
    assert_eq!(t.row_status(1).0, "unconfirmed");
    let mut report = Value::Null;
    for _ in 0..6 {
        report = t.h.ctx.reconciler.pass().await;
        t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(10));
    }
    assert_eq!(t.row_status(1).0, "rejected", "{}", report);
    assert_eq!(report["unresolved"], 0);
}

/// An owner holding an open order on both sides, so a lost order fact
/// opens a gap there.
struct Holder;

#[async_trait]
impl OrderOwner for Holder {
    fn name(&self) -> &'static str {
        "test-holder"
    }
    fn has_open_orders(&self, _mode: RunMode) -> bool {
        true
    }
    fn owns_order(&self, _order_id: &str) -> bool {
        false
    }
    async fn reconcile(&self, _mode: RunMode, _book: &BookSnapshot) -> usize {
        0
    }
}

/// ARCH-01 for OpenScript (maintainer decision): while the order reconciler
/// repairs a gap in order facts on a run's side (order updates were lost),
/// the run's new entries wait, as a strategy's do. An order that only
/// reduces what the run holds, and Stop, still go; once the book has been
/// read the run trades again.
#[tokio::test]
async fn new_entries_wait_while_order_facts_are_repaired() {
    let t = setup().await;
    let reconciler = t.h.ctx.reconciler.clone();
    reconciler.stop();
    *t.fake.gate.lock() = Some(reconciler.clone());
    let id = t.deploy("SBIN").await;
    t.fake.fill_at_once.store(true, Ordering::SeqCst);
    t.start(&id).await;
    t.buy(&id, 1, 3).await;
    t.h.ctx.trading.runner.pump_once(&id).await;

    // An order fact is lost while someone holds open orders.
    reconciler.register(Arc::new(Holder));
    reconciler.report_fact_loss(false);
    assert!(reconciler.gap_open(RunMode::Sandbox));

    // A new entry waits, and the script is told why.
    let v = t.buy(&id, 2, 1).await;
    assert_eq!(v["sent"], 0, "{}", v);
    let frames = t.frames(&id, 2).await;
    assert_eq!(frames.last().unwrap()["status"], "rejected");
    assert_eq!(frames.last().unwrap()["text"], ENTRIES_PAUSED);
    // A sell larger than the run holds would open a short: it waits too.
    assert_eq!(t.order(&id, 3, "sell", 4).await["sent"], 0);
    // An order that only reduces the position goes.
    assert_eq!(t.order(&id, 4, "sell", 1).await["sent"], 1);
    assert_eq!(t.fake.placed_on(RunMode::Sandbox).len(), 2);

    // The sandbox book is read: the gap closes and entries go again.
    reconciler.pass().await;
    assert!(!reconciler.gap_open(RunMode::Sandbox));
    assert_eq!(t.buy(&id, 5, 1).await["sent"], 1);

    // Stop is never held by a gap.
    t.h.ctx.trading.runner.pump_once(&id).await;
    reconciler.report_fact_loss(false);
    assert!(reconciler.gap_open(RunMode::Sandbox));
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let sent = t.fake.placed_on(RunMode::Sandbox);
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[3]["action"], "SELL");
    assert_eq!(sent[3]["quantity"], 3);
}
