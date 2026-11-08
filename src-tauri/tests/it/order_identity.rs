//! Gate A of the Codex re-audit of 2026-10-20: order identity across broker
//! accounts, engine incarnations and lost answers (NEW-TR-01, NEW-TR-03,
//! NEW-TR-04).
//!
//! Through the real runner, its page's HTTP routes, the Action Center and
//! the order reconciler, against two mock broker accounts told apart by
//! their own books: Zerodha (AB1234) and Angel One (XY9876). Every case
//! counts the calls each broker received and the quantity left, not only
//! the answers' statuses.

use crate::webui_support::{ist, req, with, H};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::{AfterSend, MockBroker, MockCall, PlaceGate};
use openalgo_desktop_lib::brokers::types::{Order, Position, Quote};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::services::broker_switch;
use openalgo_desktop_lib::services::live_owner::LiveOwner;
use openalgo_desktop_lib::services::order_reconciler::BookSnapshot;
use openalgo_desktop_lib::state::BrokerSession;
use openalgo_desktop_lib::strategy::dispatch::{BookOrder, RunMode};
use openalgo_desktop_lib::trading::runner::host::{fragment_of, RecordingHost};
use openalgo_desktop_lib::trading::runner::services::AppServices;
use openalgo_desktop_lib::trading::runner::RunnerOptions;
use openalgo_desktop_lib::trading::scripts::source_hash;
use openalgo_desktop_lib::trading::Trading;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// The deployment's instrument and product, as both brokers book it.
const SBIN: &str = "SBIN";

fn order(id: &str, side: &str, quantity: i32, filled: i32, status: &str) -> Order {
    Order {
        order_id: id.into(),
        exchange_order_id: None,
        symbol: SBIN.into(),
        exchange: "NSE".into(),
        side: side.into(),
        quantity,
        filled_quantity: filled,
        pending_quantity: quantity - filled,
        price: 0.0,
        trigger_price: 0.0,
        average_price: if filled > 0 { 100.0 } else { 0.0 },
        order_type: "MARKET".into(),
        product: "MIS".into(),
        status: status.into(),
        validity: "DAY".into(),
        order_timestamp: "2026-10-05 10:00:05".into(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }
}

fn long(quantity: i32) -> Position {
    Position {
        symbol: SBIN.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity,
        average_price_basis: None,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 100.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: quantity,
        buy_value: 100.0 * f64::from(quantity),
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

fn fast() -> RunnerOptions {
    RunnerOptions {
        heartbeat_timeout: Duration::from_secs(60),
        order_poll: Duration::from_secs(3600),
        bar_poll: Duration::from_secs(3600),
        close_wait: Duration::from_millis(400),
        close_look: Duration::from_millis(10),
        inbox_wait: Duration::from_millis(30),
        schedule_tick: Duration::from_secs(15),
        history_days: 5,
    }
}

/// The app with two broker accounts behind it: Zerodha (AB1234) signed in,
/// Angel One (XY9876) ready to switch to. Analyzer mode is off.
struct Two {
    h: H,
    cookie: String,
    csrf: String,
    host: Arc<RecordingHost>,
    zerodha: Arc<MockBroker>,
    angel: Arc<MockBroker>,
}

async fn two_accounts(semi_auto: bool) -> Two {
    let symbols = SymbolResolver::new();
    let zerodha = Arc::new(MockBroker::with_symbols("zerodha", symbols.clone()));
    let angel = Arc::new(MockBroker::with_symbols("angel", symbols.clone()));
    let h = H::with_brokers(
        ist(2026, 10, 5, 10, 0),
        Arc::new(BrokerRegistry::with_symbols(
            symbols,
            vec![
                zerodha.clone() as Arc<dyn Broker>,
                angel.clone() as Arc<dyn Broker>,
            ],
        )),
    );
    h.ctx.load_symbol_cache(vec![SymToken {
        symbol: SBIN.into(),
        brsymbol: "SBIN-EQ".into(),
        name: "STATE BANK OF INDIA".into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: "3045".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }]);
    for mock in [&zerodha, &angel] {
        mock.set_quote(Quote {
            symbol: SBIN.into(),
            exchange: "NSE".into(),
            ltp: 100.0,
            close: 100.0,
            ..Default::default()
        });
    }
    h.setup();
    // Installed directly: a sign-in's BrokerConnected would restart the
    // order reconciler after this harness stops it, and book reads here are
    // counted by hand.
    h.ctx
        .set_broker_session(Some(session(&h, "zerodha", "AB1234")));
    h.ctx.sqlite.set_analyze_mode(false).unwrap();
    if semi_auto {
        ApiKeyService::set_order_mode(&h.ctx, "semi_auto").unwrap();
    }
    // Book reads are driven by hand, so every one is counted.
    h.ctx.reconciler.stop();
    let (cookie, csrf) = h.session(true);
    let host = Arc::new(RecordingHost::new());
    h.ctx.trading.runner.set_host(host.clone());
    h.ctx.trading.runner.set_options(fast());
    Two {
        h,
        cookie,
        csrf,
        host,
        zerodha,
        angel,
    }
}

fn session(h: &H, broker: &str, account: &str) -> BrokerSession {
    BrokerSession {
        broker_id: broker.into(),
        auth_token: "mock-access-token".into(),
        feed_token: None,
        user_id: account.into(),
        user_name: None,
        authenticated_at: h.ctx.now(),
    }
}

fn placed(mock: &MockBroker) -> Vec<(String, i64)> {
    mock.calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::PlaceOrder(o) => Some((o.action.as_str().to_string(), o.quantity)),
            _ => None,
        })
        .collect()
}

fn cancelled(mock: &MockBroker) -> Vec<String> {
    mock.calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::CancelOrder(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// Every call that reads or changes something at the broker.
fn touched(mock: &MockBroker) -> usize {
    mock.calls()
        .iter()
        .filter(|c| {
            matches!(
                c,
                MockCall::PlaceOrder(_)
                    | MockCall::CancelOrder(_)
                    | MockCall::OrderBook
                    | MockCall::TradeBook
                    | MockCall::Positions
                    | MockCall::Holdings
            )
        })
        .count()
}

fn pairs(v: &[(&str, i64)]) -> Vec<(String, i64)> {
    v.iter().map(|(a, q)| (a.to_string(), *q)).collect()
}

impl Two {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let token = (method != Method::GET).then_some(self.csrf.as_str());
        self.h
            .json(with(req(method, path, body), &self.cookie, token))
            .await
    }

    /// Save the script and deploy it on SBIN; answers the deployment.
    async fn deploy(&self) -> String {
        self.deploy_script("t").await
    }

    /// [`Self::deploy`] for the script `name`.oscript.
    async fn deploy_script(&self, name: &str) -> String {
        let source = format!(
            "version 1\nstrategy(\"{}\", qty = 1)\nif close > open\n    buy()\n",
            name
        );
        let program = json!({"source": {"hash": source_hash(&source)}}).to_string();
        let (s, _) = self
            .call(
                Method::POST,
                &format!("/openscript/{}.oscript", name),
                Some(json!({"source": source, "program": program})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        let (s, v) = self
            .call(
                Method::POST,
                &format!("/openscript/runner/config/{}.oscript", name),
                Some(
                    json!({"symbol": SBIN, "exchange": "NSE", "interval": "1m", "product": "MIS"}),
                ),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        v["settings"]["deployment"].as_str().unwrap().to_string()
    }

    /// Start the deployment; answers its run's incarnation.
    async fn start(&self, run: &str) -> i64 {
        let (s, v) = self
            .call(
                Method::POST,
                &format!("/openscript/runner/start/{}", run),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{}", v);
        assert_eq!(v["run"]["mode"], "live", "{}", v);
        v["run"]["incarnation"].as_i64().unwrap()
    }

    async fn pause(&self, run: &str) {
        let (s, v) = self
            .call(
                Method::POST,
                &format!("/openscript/runner/pause/{}", run),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
    }

    async fn close(&self, run: &str) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            &format!("/openscript/runner/close/{}", run),
            None,
        )
        .await
    }

    async fn status(&self, run: &str) -> Value {
        self.call(
            Method::GET,
            &format!("/openscript/runner/status/{}", run),
            None,
        )
        .await
        .1
    }

    fn token(&self, run: &str) -> String {
        fragment_of(&self.host.last_launch(run).unwrap().page)
            .unwrap()
            .1
    }

    /// One request of the run's page, with the secret of its latest start.
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

    /// One confirmed bar's intents, as the page posts them.
    async fn post(&self, run: &str, incarnation: i64, intents: Value) -> (StatusCode, Value) {
        self.page(
            Method::POST,
            run,
            "intents",
            Some(json!({"confirmed": true, "incarnation": incarnation, "intents": intents})),
        )
        .await
    }

    async fn buy(&self, run: &str, incarnation: i64, intent: i64, qty: i64) -> Value {
        let (s, v) = self.post(run, incarnation, json!([buy(intent, qty)])).await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        v
    }

    /// Everything in the page's inbox.
    async fn inbox(&self, run: &str) -> Vec<Value> {
        let (_, v) = self.page(Method::GET, run, "inbox?after=0", None).await;
        v["messages"].as_array().cloned().unwrap_or_default()
    }

    async fn frames(&self, run: &str, intent: i64) -> Vec<Value> {
        self.inbox(run)
            .await
            .into_iter()
            .filter(|m| m["kind"] == "frame" && m["frame"]["intentId"] == intent)
            .map(|m| m["frame"].clone())
            .collect()
    }

    /// Replace the session some other way than a sign-in through the guard.
    fn sign_in(&self, broker: &str, account: &str) {
        self.h
            .ctx
            .set_broker_session(Some(session(&self.h, broker, account)));
    }

    /// (status, orderid, filled) of every order row, oldest first.
    fn rows(&self) -> Vec<(String, Option<String>, i64)> {
        let conn = self.h.ctx.sqlite.conn().unwrap();
        let mut st = conn
            .prepare("SELECT status, orderid, filled_quantity FROM openscript_orders ORDER BY id")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn sql(&self, text: &str) {
        self.h
            .ctx
            .sqlite
            .conn()
            .unwrap()
            .execute_batch(text)
            .unwrap();
    }
}

fn buy(intent: i64, qty: i64) -> Value {
    json!({
        "intentId": intent, "kind": "place", "side": "buy", "qty": qty, "qtyType": "units",
        "type": "market", "tag": "long", "instrument": {"symbol": SBIN, "exchange": "NSE"},
        "product": "", "positionRef": 1, "bar": {"index": 3, "time": 0}
    })
}

/// One read of a mock's order book through Zerodha (AB1234), as the
/// reconciler hands it to owners.
fn book_of(mock: &MockBroker, read_at: chrono::DateTime<chrono::Utc>) -> BookSnapshot {
    let rows = match mock.order_book.lock().clone() {
        Some(Ok(rows)) => rows,
        _ => Vec::new(),
    };
    let rows = rows
        .iter()
        .filter_map(|o| {
            BookOrder::from_row(&json!({
                "orderid": o.order_id, "symbol": o.symbol, "exchange": o.exchange,
                "action": o.side, "quantity": o.quantity, "product": o.product,
                "pricetype": o.order_type, "order_status": o.status,
                "filled_quantity": o.filled_quantity, "average_price": o.average_price,
                "timestamp": o.order_timestamp,
            }))
        })
        .collect();
    BookSnapshot::new(rows, read_at).with_session(Some(LiveOwner::new("zerodha", "AB1234")))
}

// ------------------------------------------------- NEW-TR-01: destination

/// NEW-TR-01, the re-audit's trigger: on Zerodha (AB1234) the deployment
/// buys 3 and is paused; the ordinary switch guard lets the trader switch;
/// on Angel One (XY9876), which holds an unrelated 10, it is started and
/// stopped. Before, the Zerodha fill was in its ledger and Angel's 10 let
/// the cap through, so Stop sold 3 at Angel. Now Angel receives nothing:
/// no order, no cancel, no read; the Zerodha position stays, reported; and
/// a run back on Zerodha (AB1234) closes exactly it.
#[tokio::test]
async fn a_paused_run_never_closes_another_brokers_position() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let first = t.start(&id).await;
    assert_eq!(t.buy(&id, first, 1, 3).await["sent"], 1);
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 3, "complete")]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    runner.pump_once(&id).await;
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-1".into()), 3));
    t.pause(&id).await;
    assert_eq!(
        broker_switch::refusal(&t.h.ctx, "zerodha"),
        None,
        "nothing runs: the ordinary switch is allowed"
    );

    t.sign_in("angel", "XY9876");
    *t.angel.positions.lock() = Some(Ok(vec![long(10)]));
    let second = t.start(&id).await;
    assert!(second > first, "a new run, a new incarnation");
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let answer = v["message"].as_str().unwrap_or_default();
    assert!(
        answer.contains("long 3 SBIN MIS through Zerodha (AB1234)"),
        "{}",
        answer
    );
    assert_eq!(
        v["held_elsewhere"].as_array().map(Vec::len),
        Some(1),
        "{}",
        v
    );
    assert!(placed(&t.angel).is_empty(), "no order at Angel");
    assert!(cancelled(&t.angel).is_empty(), "no cancel at Angel");
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
    let elsewhere = t.status(&id).await["elsewhere"].clone();
    assert!(
        elsewhere[0]
            .as_str()
            .unwrap_or_default()
            .contains("Zerodha (AB1234)"),
        "{}",
        elsewhere
    );

    // Back on its own account, a run there closes exactly the 3.
    t.sign_in("zerodha", "AB1234");
    *t.zerodha.order_book.lock() = Some(Ok(vec![
        order("MOCK-1", "BUY", 3, 3, "complete"),
        order("MOCK-2", "SELL", 3, 3, "complete"),
    ]));
    t.start(&id).await;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 3)]));
    assert!(placed(&t.angel).is_empty());
    assert_eq!(touched(&t.angel), 0, "Angel was never read either");
}

/// NEW-TR-01 with the same broker and another account: the session comes
/// back as Zerodha (ZZ0001). The run started there owns nothing of AB1234.
#[tokio::test]
async fn another_account_at_the_same_broker_is_another_destination() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let first = t.start(&id).await;
    t.buy(&id, first, 1, 3).await;
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 3, "complete")]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    runner.pump_once(&id).await;
    t.pause(&id).await;

    t.sign_in("zerodha", "ZZ0001");
    t.start(&id).await;
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        placed(&t.zerodha),
        pairs(&[("BUY", 3)]),
        "nothing sent as ZZ0001"
    );
    assert!(cancelled(&t.zerodha).is_empty());

    t.sign_in("zerodha", "AB1234");
    *t.zerodha.order_book.lock() = Some(Ok(vec![
        order("MOCK-1", "BUY", 3, 3, "complete"),
        order("MOCK-2", "SELL", 3, 3, "complete"),
    ]));
    t.start(&id).await;
    assert_eq!(t.close(&id).await.0, StatusCode::OK);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 3)]));
}

/// NEW-TR-01: the session is replaced under a running run some other way
/// than the switch. Nothing of the run is read, sent, cancelled or exited
/// through Angel; with Zerodha (AB1234) back, Stop closes exactly the 3.
#[tokio::test]
async fn a_session_replaced_under_a_running_run_reaches_nothing() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    t.buy(&id, run, 1, 3).await;
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 0, "open")]));
    runner.pump_once(&id).await;

    t.sign_in("angel", "XY9876");
    *t.angel.positions.lock() = Some(Ok(vec![long(10)]));
    runner.pump_once(&id).await;
    let v = t.buy(&id, run, 2, 1).await;
    assert_eq!(v["sent"], 0, "{}", v);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(
        v["message"]
            .as_str()
            .unwrap_or_default()
            .contains("opened on Zerodha (AB1234)"),
        "{}",
        v
    );
    assert_eq!(touched(&t.angel), 0, "no read, order or cancel at Angel");

    t.sign_in("zerodha", "AB1234");
    *t.zerodha.order_book.lock() = Some(Ok(vec![
        order("MOCK-1", "BUY", 3, 3, "complete"),
        order("MOCK-2", "SELL", 3, 3, "complete"),
    ]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 3)]));
    assert_eq!(touched(&t.angel), 0);
}

/// NEW-TR-01, coordinator decision: a Live row from before this version
/// recorded the account belongs to no run. It is reported in the run's
/// status and in the Stop answer with what to do; nothing is sent for it.
#[tokio::test]
async fn a_row_from_before_accounts_were_recorded_is_reported_never_traded() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    t.sql(&format!(
        "INSERT INTO openscript_orders (deployment, mode, intent_id, symbol, exchange, action,
            quantity, pricetype, product, orderid, status, filled_quantity, average_price,
            placed_at, updated_at)
         VALUES ('{}', 'live', 1, 'SBIN', 'NSE', 'BUY', 3, 'MARKET', 'MIS', 'OLD-1', 'filled',
            3, 100.0, '2026-10-01T04:00:00+00:00', '2026-10-01T04:00:00+00:00')",
        id
    ));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    t.start(&id).await;
    let elsewhere = t.status(&id).await["elsewhere"].clone();
    let line = elsewhere[0].as_str().unwrap_or_default().to_string();
    assert!(
        line.contains("placed before this version recorded"),
        "{}",
        line
    );
    assert!(line.contains("Close it at your broker"), "{}", line);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(v["message"]
        .as_str()
        .unwrap_or_default()
        .contains("Close it at your broker"));
    assert!(placed(&t.zerodha).is_empty(), "nothing sent for it");
    assert!(cancelled(&t.zerodha).is_empty());
}

/// NEW-TR-01 with LOG-08: an entry on Zerodha (AB1234) is unconfirmed when
/// the session becomes Angel One (XY9876), whose book happens to hold a
/// matching BUY 3. Angel's book is not read for it and nothing binds it
/// there; back on Zerodha (AB1234) its own book settles it.
#[tokio::test]
async fn an_unconfirmed_order_is_settled_only_by_its_own_brokers_book() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let run = t.start(&id).await;
    t.zerodha
        .after_send
        .lock()
        .push_back((AfterSend::Dropped, "complete"));
    *t.zerodha.after_send_price.lock() = 100.0;
    *t.zerodha.after_send_time.lock() = "2026-10-05 10:00:05".into();
    t.buy(&id, run, 1, 3).await;
    assert_eq!(t.rows()[0].0, "unconfirmed");
    t.pause(&id).await;

    t.sign_in("angel", "XY9876");
    *t.angel.order_book.lock() = Some(Ok(vec![order("ANGEL-7", "BUY", 3, 3, "complete")]));
    t.h.ctx.reconciler.pass().await;
    assert_eq!(t.rows()[0], ("unconfirmed".into(), None, 0));
    assert_eq!(touched(&t.angel), 0, "Angel's book is not read for it");

    t.sign_in("zerodha", "AB1234");
    t.h.ctx.reconciler.pass().await;
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-1".into()), 3));
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
}

// ------------------------------------------------ NEW-TR-03: incarnation

/// NEW-TR-03: run A's working intent 1 fills after Pause and restart, while
/// run B's engine has its own intent 1. A's fill never reaches B's page,
/// B's cancel of its tag never touches A's order, and Stop closes what the
/// deployment holds on the account: A's 3, with B's order cancelled unfilled.
#[tokio::test]
async fn an_earlier_runs_fill_never_reaches_a_new_engine() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let a = t.start(&id).await;
    t.buy(&id, a, 1, 3).await;
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 0, "open")]));
    runner.pump_once(&id).await;
    t.pause(&id).await;

    let b = t.start(&id).await;
    assert_eq!(t.buy(&id, b, 1, 1).await["sent"], 1);
    *t.zerodha.order_book.lock() = Some(Ok(vec![
        order("MOCK-1", "BUY", 3, 0, "open"),
        order("MOCK-2", "BUY", 1, 0, "open"),
    ]));
    runner.pump_once(&id).await;

    // B's engine cancels its tag while A's order of that tag still works:
    // only B's own order is cancelled.
    let (s, v) = t
        .post(
            &id,
            b,
            json!([{"intentId": 2, "kind": "cancel", "tag": "long"}]),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(cancelled(&t.zerodha), vec!["MOCK-2".to_string()]);

    // A's order fills late; B's ends cancelled.
    *t.zerodha.order_book.lock() = Some(Ok(vec![
        order("MOCK-1", "BUY", 3, 3, "complete"),
        order("MOCK-2", "BUY", 1, 0, "cancelled"),
    ]));
    runner.pump_once(&id).await;
    let inbox = t.inbox(&id).await;
    assert!(inbox.iter().all(|m| m["incarnation"] == b), "{:?}", inbox);
    let frames = t.frames(&id, 1).await;
    assert!(!frames.is_empty());
    assert!(
        frames.iter().all(|f| f["orderRef"] != "MOCK-1"),
        "A's fill reached B's engine: {:?}",
        frames
    );
    assert_eq!(
        t.rows()[0],
        ("filled".into(), Some("MOCK-1".into()), 3),
        "A's fill counted"
    );
    assert_eq!(
        frames.last().unwrap()["status"],
        "cancelled",
        "B hears of its own"
    );

    *t.zerodha.order_book.lock() = Some(Ok(vec![
        order("MOCK-1", "BUY", 3, 3, "complete"),
        order("MOCK-2", "BUY", 1, 0, "cancelled"),
        order("MOCK-3", "SELL", 3, 3, "complete"),
    ]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        placed(&t.zerodha),
        pairs(&[("BUY", 3), ("BUY", 1), ("SELL", 3)])
    );
}

/// NEW-TR-03 with LOG-08: run A's entry is unconfirmed when it is paused.
/// Run B's first entry waits for the book; the reconciler binds A's row and
/// B's page is not told. B's page asking again about its refused intent is
/// refused again, though the book has settled: never placed late.
#[tokio::test]
async fn a_late_bind_of_an_earlier_runs_order_reaches_no_new_engine() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let a = t.start(&id).await;
    t.zerodha
        .after_send
        .lock()
        .push_back((AfterSend::Dropped, "complete"));
    *t.zerodha.after_send_price.lock() = 100.0;
    *t.zerodha.after_send_time.lock() = "2026-10-05 10:00:05".into();
    t.buy(&id, a, 1, 3).await;
    assert_eq!(t.rows()[0].0, "unconfirmed");
    t.pause(&id).await;

    let b = t.start(&id).await;
    let v = t.buy(&id, b, 1, 1).await;
    assert_eq!(
        (v["sent"].clone(), v["unconfirmed"].clone()),
        (json!(0), json!(true))
    );
    t.h.ctx.reconciler.pass().await;
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-1".into()), 3));
    let frames = t.frames(&id, 1).await;
    assert_eq!(frames.len(), 1, "{:?}", frames);
    assert_eq!(frames[0]["status"], "rejected");
    assert!(frames[0]["orderRef"].is_null());

    // The page asks again about its intent 1: the same refusal, not an order.
    let v = t.buy(&id, b, 1, 1).await;
    assert_eq!(
        (v["sent"].clone(), v["repeated"].clone()),
        (json!(0), json!(1))
    );
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
    assert_eq!(t.frames(&id, 1).await[0]["status"], "rejected");

    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    t.zerodha
        .order_book
        .lock()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .push(order("MOCK-2", "SELL", 3, 3, "complete"));
    assert_eq!(t.close(&id).await.0, StatusCode::OK);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 3)]));
}

// ------------------------------------------------ NEW-TR-04: lost answers

/// NEW-TR-04 through the page's HTTP route: the answer to a BUY is lost and
/// the page sends the same batch again; two copies of one batch arrive at
/// once. Each intent is placed once, and the repeat is answered with the
/// order it became.
#[tokio::test]
async fn a_batch_sent_again_never_places_a_second_entry() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let run = t.start(&id).await;
    let first = t.buy(&id, run, 1, 3).await;
    assert_eq!(first["sent"], 1);
    // The page never read that answer, so it sends the batch again.
    let again = t.buy(&id, run, 1, 3).await;
    assert_eq!(
        (again["sent"].clone(), again["repeated"].clone()),
        (json!(0), json!(1))
    );
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
    let last = t.frames(&id, 1).await.last().cloned().unwrap();
    assert_eq!(
        (last["status"].clone(), last["orderRef"].clone()),
        (json!("working"), json!("MOCK-1"))
    );

    let (one, two) = tokio::join!(t.buy(&id, run, 2, 1), t.buy(&id, run, 2, 1));
    let sent = one["sent"].as_i64().unwrap() + two["sent"].as_i64().unwrap();
    assert_eq!(sent, 1, "{} {}", one, two);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("BUY", 1)]));
}

/// NEW-TR-04: the page's request is dropped while its BUY is at the broker.
/// The batch finishes anyway: the order is recorded with its number, and
/// the page asking again is answered from it. Before, the placement was cut
/// off with the request, leaving a row without its answer.
#[tokio::test]
async fn a_dropped_request_still_records_its_placement() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let run = t.start(&id).await;
    let gate = Arc::new(PlaceGate::default());
    *t.zerodha.place_gate.lock() = Some(gate.clone());
    {
        let request = t.post(&id, run, json!([buy(1, 3)]));
        tokio::select! {
            _ = request => panic!("the broker gate holds the placement"),
            _ = gate.entered.notified() => {}
        }
        // The page's request is gone here; its placement is at the broker.
    }
    gate.release.notify_one();
    let mut recorded = false;
    for _ in 0..200 {
        if t.rows()[0].1.is_some() {
            recorded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(recorded, "the placement's answer was saved: {:?}", t.rows());
    assert_eq!(t.rows()[0], ("working".into(), Some("MOCK-1".into()), 0));
    *t.zerodha.place_gate.lock() = None;
    let again = t.buy(&id, run, 1, 3).await;
    assert_eq!(again["repeated"], 1, "{}", again);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
}

/// NEW-TR-04 with Semi-Auto (LOG-04) and a partial fill: the held BUY is
/// sent again by the page (one Action Center order, no broker call), then
/// approved and partly filled. Under another account Stop reaches nothing;
/// on its own it cancels the rest and sells exactly the 2 filled.
#[tokio::test]
async fn a_held_order_sent_again_is_held_once_and_its_partial_fill_closed_exactly() {
    let t = two_accounts(true).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    assert_eq!(t.buy(&id, run, 1, 3).await["sent"], 1);
    let again = t.buy(&id, run, 1, 3).await;
    assert_eq!(again["repeated"], 1, "{}", again);
    let last = t.frames(&id, 1).await.last().cloned().unwrap();
    assert_eq!(
        last["status"], "placed",
        "held is pending, not an acceptance"
    );
    let (pending, held): (i64, i64) = t
        .h
        .ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT (SELECT pending_order_id FROM openscript_orders), (SELECT COUNT(*) FROM pending_orders)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(held, 1, "held once in the Action Center");
    assert!(placed(&t.zerodha).is_empty());

    let (s, v) = t
        .call(
            Method::POST,
            &format!("/action-center/approve/{}", pending),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 2, "open")]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(2)]));
    runner.pump_once(&id).await;
    assert_eq!(t.rows()[0].1.as_deref(), Some("MOCK-1"));
    assert_eq!(t.rows()[0].2, 2);

    t.sign_in("angel", "XY9876");
    assert_eq!(t.close(&id).await.0, StatusCode::CONFLICT);
    assert_eq!(touched(&t.angel), 0);

    t.sign_in("zerodha", "AB1234");
    // The broker ends the order with 2 filled once the cancel arrives.
    let mock = t.zerodha.clone();
    let broker = tokio::spawn(async move {
        for _ in 0..400 {
            if cancelled(&mock).iter().any(|c| c == "MOCK-1") {
                *mock.order_book.lock() = Some(Ok(vec![
                    order("MOCK-1", "BUY", 3, 2, "cancelled"),
                    order("MOCK-2", "SELL", 2, 2, "complete"),
                ]));
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let (s, v) = t.close(&id).await;
    broker.await.unwrap();
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(cancelled(&t.zerodha), vec!["MOCK-1".to_string()]);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 2)]));
    assert_eq!(touched(&t.angel), 0);
}

/// NEW-TR-04 across a crash: the BUY reaches the broker, its answer cannot
/// be saved, the page asks again (answered from the row), and the app dies.
/// Reopened, the old page is gone; a request naming the old run is answered
/// from its record; the new run's own entry waits for the order book; the
/// book settles the old row and Stop sells exactly 3. One BUY in all.
#[tokio::test]
async fn an_unsaved_answer_then_a_crash_and_reopen_place_exactly_once() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let old = t.start(&id).await;
    t.sql(
        "CREATE TRIGGER no_ack BEFORE UPDATE OF orderid ON openscript_orders
         BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
    );
    assert_eq!(t.buy(&id, old, 1, 3).await["sent"], 1);
    assert_eq!(
        t.rows()[0],
        ("placed".into(), None, 0),
        "its answer unsaved"
    );
    let again = t.buy(&id, old, 1, 3).await;
    assert_eq!(again["repeated"], 1, "{}", again);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
    let old_token = t.token(&id);

    // The app dies before the answer is saved; it is reopened on the same
    // data. Its first start marks the unanswered row unconfirmed.
    t.sql("DROP TRIGGER no_ack;");
    let reopened = Trading::new(
        t.h.dir.path(),
        t.h.ctx.sqlite.clone(),
        t.h.clock.clone(),
        Arc::new(AppServices::new(Arc::downgrade(&t.h.ctx))),
    );
    reopened.start(t.h.ctx.now(), &t.h.ctx.sqlite);
    let runner = reopened.runner.clone();
    runner.set_host(t.host.clone());
    runner.set_options(fast());
    assert_eq!(t.rows()[0].0, "unconfirmed");
    let intents = vec![buy(1, 3)];
    assert!(
        runner
            .page_intents(&id, &old_token, Some(old), intents.clone())
            .await
            .is_none(),
        "the old page's run is gone"
    );

    let new = runner.start(&id).unwrap().incarnation;
    assert!(new > old);
    let token = t.token(&id);
    let stale = runner
        .page_intents(&id, &token, Some(old), intents.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale["stale"], true, "{}", stale);
    assert_eq!(
        stale["outcomes"][0]["status"], "placed",
        "unknown, never rejected"
    );
    let own = runner
        .page_intents(&id, &token, Some(new), intents.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (own["sent"].clone(), own["unconfirmed"].clone()),
        (json!(0), json!(true))
    );
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));

    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 3, "complete")]));
    let book = book_of(&t.zerodha, t.h.ctx.now() + chrono::Duration::seconds(10));
    assert_eq!(
        runner.reconcile_book(RunMode::Live, &book).await.unwrap(),
        0
    );
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-1".into()), 3));
    let stale = runner
        .page_intents(&id, &token, Some(old), intents)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale["outcomes"][0]["status"], "filled");

    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    t.zerodha
        .order_book
        .lock()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .push(order("MOCK-2", "SELL", 3, 3, "complete"));
    runner.close(&id).await.unwrap();
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 3)]));
    reopened.shutdown().await;
}

/// Security review of #165: a page that names another deployment's run
/// number with a real intent id is answered "not sent" and reads none of
/// that deployment's orders. Before, the lookup took the run number alone
/// and handed back the other deployment's order (its number and status).
#[tokio::test]
async fn a_page_naming_another_deployments_run_reads_none_of_its_orders() {
    let t = two_accounts(false).await;
    let mine = t.deploy_script("t").await;
    let theirs = t.deploy_script("u").await;
    t.start(&mine).await;
    let their_run = t.start(&theirs).await;
    assert_eq!(t.buy(&theirs, their_run, 1, 3).await["sent"], 1);
    let (s, v) = t.post(&mine, their_run, json!([buy(1, 3)])).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["stale"], true, "{}", v);
    let outcome = &v["outcomes"][0];
    assert_eq!(outcome["status"], "rejected", "{}", v);
    assert!(outcome["orderRef"].is_null(), "no order of theirs: {}", v);
    assert_eq!(outcome["filledQty"], 0);
    assert_eq!(
        placed(&t.zerodha),
        pairs(&[("BUY", 3)]),
        "nothing placed for it"
    );
}

// ------------------------------------------- independent review of #165

/// Review 1: a working order at another account is told, never followed or
/// cancelled from here. Zerodha (AB1234) BUY 3 left open and a working row
/// from before accounts were recorded; started on Angel One (XY9876), the
/// status and the Stop answer name both, and nothing reaches either broker.
#[tokio::test]
async fn a_working_order_at_another_account_is_told_never_followed() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    t.buy(&id, run, 1, 3).await;
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 0, "open")]));
    runner.pump_once(&id).await;
    t.pause(&id).await;
    t.sql(&format!(
        "INSERT INTO openscript_orders (deployment, mode, intent_id, symbol, exchange, action,
            quantity, pricetype, product, orderid, status, filled_quantity, placed_at, updated_at)
         VALUES ('{}', 'live', 1, 'SBIN', 'NSE', 'BUY', 2, 'LIMIT', 'MIS', 'OLD-2', 'working',
            0, '2026-10-01T04:00:00+00:00', '2026-10-01T04:00:00+00:00')",
        id
    ));
    t.sign_in("angel", "XY9876");
    t.start(&id).await;
    let elsewhere = t.status(&id).await["elsewhere"].to_string();
    assert!(
        elsewhere.contains(
            "may still be working through Zerodha (AB1234): BUY 3 SBIN MIS (order MOCK-1)"
        ),
        "{}",
        elsewhere
    );
    assert!(
        elsewhere.contains("BUY 2 SBIN MIS (order OLD-2)"),
        "{}",
        elsewhere
    );
    assert!(
        elsewhere.contains("cancel it at your broker"),
        "{}",
        elsewhere
    );
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["held_elsewhere"].as_array().map(Vec::len),
        Some(2),
        "{}",
        v
    );
    assert_eq!(touched(&t.angel), 0);
    assert!(
        cancelled(&t.zerodha).is_empty(),
        "nothing cancelled from here"
    );
}

/// Review 3: a session at the same broker whose account is not known owns
/// nothing: no order, no cancel, no exit of AB1234's position through it,
/// and no Live start on it.
#[tokio::test]
async fn a_session_whose_account_is_not_known_owns_nothing() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    t.buy(&id, run, 1, 3).await;
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 3, "complete")]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    runner.pump_once(&id).await;

    t.sign_in("zerodha", "");
    assert_eq!(t.buy(&id, run, 2, 1).await["sent"], 0);
    assert_eq!(t.close(&id).await.0, StatusCode::CONFLICT);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
    assert!(cancelled(&t.zerodha).is_empty());
    t.pause(&id).await;
    let (s, v) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(
        v["message"]
            .as_str()
            .unwrap_or_default()
            .contains("could not tell which broker account"),
        "{}",
        v
    );
}

/// Review 8: without a login whose account is known, a Live deployment's
/// book is unavailable, never an empty book.
#[tokio::test]
async fn a_book_without_a_known_account_is_unavailable_not_empty() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    t.buy(&id, run, 1, 3).await;
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 3, "complete")]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    runner.pump_once(&id).await;
    t.pause(&id).await;
    t.sign_in("zerodha", "");
    for book in ["positions", "orderbook", "tradebook"] {
        let (s, v) = t
            .call(
                Method::GET,
                &format!("/openscript/runner/{}/{}", book, id),
                None,
            )
            .await;
        assert_ne!(s, StatusCode::OK, "{}: {}", book, v);
        assert!(
            v["message"]
                .as_str()
                .unwrap_or_default()
                .contains("cannot be read"),
            "{}",
            v
        );
    }
    // Its status says so too, never an empty list.
    let elsewhere = t.status(&id).await["elsewhere"].to_string();
    assert!(elsewhere.contains("cannot be read"), "{}", elsewhere);
}

/// Review 4: a batch at the broker is a task the runner owns. Shutdown
/// waits for it, its answer is saved, and no task is left.
#[tokio::test]
async fn shutdown_waits_for_a_batch_at_the_broker_and_leaves_no_task() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    let gate = Arc::new(PlaceGate::default());
    *t.zerodha.place_gate.lock() = Some(gate.clone());
    {
        let request = t.post(&id, run, json!([buy(1, 3)]));
        tokio::select! {
            _ = request => panic!("the broker gate holds the placement"),
            _ = gate.entered.notified() => {}
        }
    }
    assert!(
        runner.task_count() >= 1,
        "the batch is the runner's own task"
    );
    let shutdown = runner.shutdown();
    tokio::pin!(shutdown);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut shutdown)
            .await
            .is_err(),
        "shutdown waits for the order at the broker"
    );
    gate.release.notify_one();
    shutdown.await;
    assert_eq!(runner.task_count(), 0);
    assert_eq!(t.rows()[0], ("working".into(), Some("MOCK-1".into()), 0));
}

/// Review 10: the page sends a batch again while the first copy is still at
/// the broker. The second waits for the first and is answered from its
/// record: one order.
#[tokio::test]
async fn a_batch_sent_again_while_the_first_is_at_the_broker_places_once() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let run = t.start(&id).await;
    let gate = Arc::new(PlaceGate::default());
    *t.zerodha.place_gate.lock() = Some(gate.clone());
    let first = t.post(&id, run, json!([buy(1, 3)]));
    let second = async {
        gate.entered.notified().await;
        let again = t.post(&id, run, json!([buy(1, 3)]));
        tokio::pin!(again);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut again)
                .await
                .is_err(),
            "the copy waits behind the batch at the broker"
        );
        gate.release.notify_one();
        again.await
    };
    let ((s1, v1), (s2, v2)) = tokio::join!(first, second);
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK), "{} {}", v1, v2);
    assert_eq!(v1["sent"], 1, "{}", v1);
    assert_eq!(v2["repeated"], 1, "{}", v2);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));
}

/// Review 5: a batch with an intent that has no usable number, or two of the
/// same number, is refused whole before anything is sent.
#[tokio::test]
async fn a_batch_with_a_missing_or_repeated_number_sends_nothing() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let run = t.start(&id).await;
    let mut unnumbered = buy(1, 3);
    unnumbered.as_object_mut().unwrap().remove("intentId");
    for batch in [
        json!([unnumbered]),
        json!([buy(-1, 3)]),
        json!([buy(1, 3), buy(1, 2)]),
    ] {
        let (s, v) = t.post(&id, run, batch.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{} -> {}", batch, v);
    }
    assert!(placed(&t.zerodha).is_empty());
    assert!(t.rows().is_empty());
}

/// Review 6: a run that never began is not kept, and a removed deployment's
/// runs go with it.
#[tokio::test]
async fn runs_are_forgotten_with_their_deployment_and_when_they_never_begin() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    let runs = || -> i64 {
        t.h.ctx
            .sqlite
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM openscript_incarnations WHERE deployment = ?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap()
    };
    t.start(&id).await;
    t.pause(&id).await;
    assert_eq!(runs(), 1);
    t.host.refuse_next("the window could not be opened");
    let (s, _) = t
        .call(
            Method::POST,
            &format!("/openscript/runner/start/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(runs(), 1, "the start that never began left no run");
    let (s, v) = t
        .call(
            Method::DELETE,
            &format!("/openscript/runner/config/{}", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(runs(), 0);
}

/// Review 7: a page whose batch has waited about a minute says so; the
/// trader is told once, in the run's log and on the Health page, and the
/// run's status shows it waiting until the page has its answer.
#[tokio::test]
async fn a_page_waiting_too_long_tells_the_trader_once() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    t.start(&id).await;
    let log = |lines: &[&str], waiting: bool, stuck: bool| {
        t.page(
            Method::POST,
            &id,
            "log",
            Some(json!({"lines": lines, "waiting": waiting, "stuck": stuck})),
        )
    };
    log(&["waiting"], true, false).await;
    assert_eq!(t.status(&id).await["run"]["awaiting_confirmation"], true);
    log(&["a minute"], true, true).await;
    log(&["still"], true, true).await;
    let path = t.status(&id).await["run"]["log"]
        .as_str()
        .unwrap()
        .to_string();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text.matches("for over a minute").count(), 1, "{}", text);
    let alerts: i64 = t
        .h
        .ctx
        .logs
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM health_alerts WHERE alert_type = 'openscript_orders' AND resolved = 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(alerts, 1);
    log(&["answered"], false, false).await;
    assert_eq!(t.status(&id).await["run"]["awaiting_confirmation"], false);
    let open: i64 = t
        .h
        .ctx
        .logs
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM health_alerts WHERE alert_type = 'openscript_orders' AND resolved = 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        open, 0,
        "the alert is resolved once the page has its answer"
    );
}

/// Review 9: a book read through Angel One, folded after Zerodha (AB1234)
/// is connected again, settles none of Zerodha's orders: the session is the
/// one that read the book, not the one connected at fold time.
#[tokio::test]
async fn a_book_settles_only_the_orders_of_the_session_that_read_it() {
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let id = t.deploy().await;
    let run = t.start(&id).await;
    t.zerodha
        .after_send
        .lock()
        .push_back((AfterSend::Dropped, "complete"));
    *t.zerodha.after_send_price.lock() = 100.0;
    *t.zerodha.after_send_time.lock() = "2026-10-05 10:00:05".into();
    t.buy(&id, run, 1, 3).await;
    assert_eq!(t.rows()[0].0, "unconfirmed");
    let read_at = t.h.ctx.now() + chrono::Duration::seconds(10);
    let through_angel =
        book_of(&t.zerodha, read_at).with_session(Some(LiveOwner::new("angel", "XY9876")));
    runner
        .reconcile_book(RunMode::Live, &through_angel)
        .await
        .unwrap();
    assert_eq!(t.rows()[0], ("unconfirmed".into(), None, 0));
    runner
        .reconcile_book(RunMode::Live, &book_of(&t.zerodha, read_at))
        .await
        .unwrap();
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-1".into()), 3));
}

/// Review 10: a Live order still unconfirmed from before accounts were
/// recorded is settled by the book of the session that finds it, which is
/// recorded as its owner.
#[tokio::test]
async fn an_unconfirmed_row_from_before_accounts_is_adopted_by_the_book_that_finds_it() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    t.sql(&format!(
        "INSERT INTO openscript_orders (deployment, mode, intent_id, symbol, exchange, action,
            quantity, pricetype, product, status, filled_quantity, placed_at, updated_at)
         VALUES ('{}', 'live', 1, 'SBIN', 'NSE', 'BUY', 3, 'MARKET', 'MIS', 'unconfirmed', 0,
            '2026-10-05T04:30:00+00:00', '2026-10-05T04:30:00+00:00')",
        id
    ));
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-9", "BUY", 3, 3, "complete")]));
    // The first pass of a session reads its book once for such a row.
    t.h.ctx.reconciler.pass().await;
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-9".into()), 3));
    let (broker, account): (String, String) =
        t.h.ctx
            .sqlite
            .conn()
            .unwrap()
            .query_row("SELECT broker, account FROM openscript_orders", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
    assert_eq!((broker.as_str(), account.as_str()), ("zerodha", "AB1234"));
}

/// Re-review 3: a Live order still unconfirmed from before accounts were
/// recorded, absent from the connected session's book, is never released or
/// held as absent: that book may not be its broker's. It stays unconfirmed
/// and the status tells the trader to check their broker.
#[tokio::test]
async fn an_unconfirmed_row_from_before_accounts_is_never_judged_absent() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    t.sql(&format!(
        "INSERT INTO openscript_orders (deployment, mode, intent_id, symbol, exchange, action,
            quantity, pricetype, product, status, filled_quantity, placed_at, updated_at)
         VALUES ('{}', 'live', 1, 'SBIN', 'NSE', 'BUY', 3, 'MARKET', 'MIS', 'unconfirmed', 0,
            '2026-10-05T04:30:00+00:00', '2026-10-05T04:30:00+00:00')",
        id
    ));
    let runner = t.h.ctx.trading.runner.clone();
    for _ in 0..8 {
        t.h.ctx.reconciler.pass().await;
        runner
            .reconcile_book(RunMode::Live, &book_of(&t.zerodha, t.h.ctx.now()))
            .await
            .unwrap();
        t.h.clock
            .set(t.h.ctx.now() + chrono::Duration::seconds(120));
    }
    assert_eq!(
        t.rows()[0],
        ("unconfirmed".into(), None, 0),
        "never released as absent"
    );
    assert_eq!(
        touched(&t.zerodha),
        1,
        "one book read for it in this session, not one per pass"
    );
    let elsewhere = t.status(&id).await["elsewhere"].to_string();
    assert!(
        elsewhere.contains("may have been placed at your broker before this version recorded"),
        "{}",
        elsewhere
    );
}

/// Coordinator (Paytm Money): a broker whose login names no account. The
/// run is owned by the saved keys the session was made from: it starts and
/// trades; with other keys saved for that broker, the session owns none of
/// its orders (nothing sent, Stop refused); with the first keys back, Stop
/// closes exactly the 3. Before, an empty account refused the start.
#[tokio::test]
async fn a_broker_that_names_no_account_is_owned_by_its_saved_keys() {
    use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
    let t = two_accounts(false).await;
    let runner = t.h.ctx.trading.runner.clone();
    let save_keys = |key: &str| {
        credentials::save(
            &t.h.ctx.sqlite.conn().unwrap(),
            &t.h.ctx.security,
            "zerodha",
            CredentialUpdate {
                api_key: Some(key.into()),
                api_secret: Some("secret".into()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    save_keys("first-key");
    t.sign_in("zerodha", "");
    let id = t.deploy().await;
    let run = t.start(&id).await;
    assert_eq!(t.buy(&id, run, 1, 3).await["sent"], 1);
    *t.zerodha.order_book.lock() = Some(Ok(vec![order("MOCK-1", "BUY", 3, 3, "complete")]));
    *t.zerodha.positions.lock() = Some(Ok(vec![long(3)]));
    runner.pump_once(&id).await;
    assert_eq!(t.rows()[0], ("filled".into(), Some("MOCK-1".into()), 3));

    // Other keys saved for the broker end its session; the next login is
    // made with them.
    save_keys("second-key");
    t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(1));
    t.sign_in("zerodha", "");
    assert_eq!(t.buy(&id, run, 2, 1).await["sent"], 0);
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(
        v["message"]
            .as_str()
            .unwrap_or_default()
            .contains("saved keys"),
        "{}",
        v
    );
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3)]));

    save_keys("first-key");
    t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(1));
    t.sign_in("zerodha", "");
    t.zerodha
        .order_book
        .lock()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .push(order("MOCK-2", "SELL", 3, 3, "complete"));
    let (s, v) = t.close(&id).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(placed(&t.zerodha), pairs(&[("BUY", 3), ("SELL", 3)]));
}

/// Save `api_key` as the broker's keys.
fn save_keys(t: &Two, broker: &str, api_key: &str) {
    use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
    credentials::save(
        &t.h.ctx.sqlite.conn().unwrap(),
        &t.h.ctx.security,
        broker,
        CredentialUpdate {
            api_key: Some(api_key.into()),
            api_secret: Some("secret".into()),
            ..Default::default()
        },
    )
    .unwrap();
}

/// The account the session connected now trades through.
fn account_now(t: &Two) -> String {
    LiveOwner::of_session(&t.h.ctx).unwrap().account
}

/// Lead's third review of #165, item 3: the account is worked out once per
/// session, so keys that cannot be read later (a locked vault, a busy
/// database) do not change it; a new session whose keys cannot be read
/// owns nothing, never the placeholder its login named.
#[tokio::test]
async fn the_account_is_worked_out_once_and_unreadable_keys_own_nothing() {
    let t = two_accounts(false).await;
    save_keys(&t, "zerodha", "kitekey");
    t.sign_in("zerodha", "");
    let first = account_now(&t);
    assert!(first.starts_with("keys:"), "{}", first);
    t.sql("ALTER TABLE broker_credentials RENAME TO broker_credentials_unreadable;");
    assert_eq!(account_now(&t), first, "the same session keeps its account");
    // A new session naming only the API key (a placeholder): unreadable
    // keys cannot tell, so it owns nothing.
    t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(1));
    t.sign_in("zerodha", "kitekey");
    assert_eq!(account_now(&t), "");
}

/// Lead's third review of #165, item 4: an XTS login names the market
/// login's account once market keys are saved, the interactive one before.
/// The account is told by the interactive keys either way, so saving market
/// keys later does not make another owner.
#[tokio::test]
async fn an_xts_login_is_told_by_its_saved_keys_whichever_login_named_it() {
    let t = two_accounts(false).await;
    save_keys(&t, "jainamxts", "interactive-key");
    t.sign_in("jainamxts", "INTERACTIVE1");
    let before = account_now(&t);
    assert!(before.starts_with("keys:"), "{}", before);
    t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(1));
    t.sign_in("jainamxts", "MARKET1");
    assert_eq!(account_now(&t), before);
}

/// Disclosed limit (lead's third review of #165, item 6): where the broker
/// names no account, two logins of different accounts made with the same
/// saved keys are one owner. Other keys are another owner.
#[tokio::test]
async fn two_logins_on_the_same_keys_are_one_owner_when_the_broker_names_neither() {
    let t = two_accounts(false).await;
    save_keys(&t, "paytm", "one-key-set");
    t.sign_in("paytm", "");
    let first = account_now(&t);
    t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(1));
    t.sign_in("paytm", "");
    assert_eq!(account_now(&t), first, "the limit: one key set, one owner");
    save_keys(&t, "paytm", "another-key-set");
    t.h.clock.set(t.h.ctx.now() + chrono::Duration::seconds(1));
    t.sign_in("paytm", "");
    assert_ne!(account_now(&t), first);
}

/// Lead's third review of #165, item 5: such a row is listed with the held
/// orders, and the trader's mark after checking the broker sets it aside.
#[tokio::test]
async fn an_unconfirmed_row_from_before_accounts_is_dismissed_by_the_trader() {
    let t = two_accounts(false).await;
    let id = t.deploy().await;
    t.sql(&format!(
        "INSERT INTO openscript_orders (deployment, mode, intent_id, symbol, exchange, action,
            quantity, pricetype, product, status, filled_quantity, placed_at, updated_at)
         VALUES ('{}', 'live', 1, 'SBIN', 'NSE', 'BUY', 3, 'MARKET', 'MIS', 'unconfirmed', 0,
            '2026-10-05T04:30:00+00:00', '2026-10-05T04:30:00+00:00')",
        id
    ));
    let runner = t.h.ctx.trading.runner.clone();
    // Listed for the trader, who checks the broker and marks it.
    let held = runner.held_orders();
    assert_eq!(held.len(), 1, "{:?}", held);
    assert!(
        held[0].message.contains("Check your broker's order book"),
        "{:?}",
        held
    );
    t.h.ctx
        .reconciler
        .confirm_not_placed("openscript", &held[0].id)
        .await
        .unwrap();
    assert_eq!(t.rows()[0].0, "rejected");
    assert!(runner.held_orders().is_empty());
    let elsewhere = t.status(&id).await["elsewhere"].to_string();
    assert!(!elsewhere.contains("may have been placed"), "{}", elsewhere);
}
