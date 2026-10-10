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

#[derive(Default)]
struct Fake {
    analyzer: AtomicBool,
    broker: AtomicBool,
    refuse_orders: AtomicBool,
    fill_at_once: AtomicBool,
    closed_market: AtomicBool,
    nobody: AtomicBool,
    next: AtomicU64,
    placed: Mutex<Vec<(RunMode, Value, bool)>>,
    statuses: Mutex<HashMap<String, Value>>,
    cancelled: Mutex<Vec<String>>,
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
        let status = if self.fill_at_once.load(Ordering::SeqCst) {
            json!({"order_status": "complete", "quantity": request["quantity"], "average_price": 100.0})
        } else {
            json!({"order_status": "open", "quantity": request["quantity"]})
        };
        self.statuses.lock().insert(id.clone(), status);
        DispatchResult {
            ok: true,
            broker_order_id: Some(id.clone()),
            response: json!({"status": "success", "orderid": id}),
            ..Default::default()
        }
    }
    async fn cancel(&self, _m: RunMode, orderid: &str) -> DispatchResult {
        self.cancelled.lock().push(orderid.to_string());
        if let Some(s) = self.statuses.lock().get_mut(orderid) {
            s["order_status"] = json!("cancelled");
        }
        DispatchResult {
            ok: true,
            broker_order_id: Some(orderid.into()),
            ..Default::default()
        }
    }
    async fn order_status(&self, _m: RunMode, orderid: &str) -> OrderStatusResult {
        match self.statuses.lock().get(orderid) {
            Some(s) => OrderStatusResult {
                ok: true,
                order: s.clone(),
                error: None,
            },
            None => OrderStatusResult::default(),
        }
    }
    async fn book(&self, mode: RunMode, book: Book) -> Result<Value, Value> {
        let rows: Vec<Value> = self
            .placed
            .lock()
            .iter()
            .enumerate()
            .filter(|(_, (m, _, _))| *m == mode)
            .map(|(i, (_, r, _))| {
                json!({"orderid": format!("OID{}", i + 1), "symbol": r["symbol"], "exchange": r["exchange"],
                       "action": r["action"], "order_status": "complete", "ltp": 110.0})
            })
            .chain(std::iter::once(json!({"orderid": "MANUAL", "symbol": "SBIN", "exchange": "NSE", "action": "BUY", "order_status": "complete"})))
            .collect();
        Ok(match book {
            Book::Orders => {
                json!({"status": "success", "data": {"orders": rows, "statistics": {"total_buy_orders": 9}}})
            }
            Book::Trades => json!({"status": "success", "data": rows}),
            Book::Positions => json!({"status": "success", "total_pnl": 999.0,
                "data": [{"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 1, "ltp": 110.0},
                         {"symbol": "INFY", "exchange": "NSE", "product": "MIS", "quantity": 5, "ltp": 1.0}]}),
        })
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
        let (s, v) = self
            .page(
                Method::POST,
                run,
                "intents",
                Some(json!({"confirmed": true, "intents": [{
                    "intentId": intent, "kind": "place", "side": "buy", "qty": qty, "qtyType": "units",
                    "type": "market", "tag": "long", "instrument": {"symbol": "SBIN", "exchange": "NSE"},
                    "product": "", "positionRef": 1, "bar": {"index": 3, "time": 0}
                }]})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        v
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
    t.fake.statuses.lock().insert(
        "OID1".into(),
        json!({"order_status": "complete", "quantity": 2, "average_price": 100.0}),
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
