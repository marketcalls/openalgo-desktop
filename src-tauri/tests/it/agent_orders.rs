//! The Agent's order tools end to end: through the real `/api/v1`
//! handlers in process, the sandbox, Semi-Auto, the mock broker, the
//! persisted risk counters, the kill switch and the audit trail, as a run
//! calls them after the trader approved.

use crate::api_v1_support::H;
use openalgo_desktop_lib::agent::contract::tool::{
    AgentTool, Capabilities, Surface, ToolCtx, ToolOutcome,
};
use openalgo_desktop_lib::agent::contract::viz::VizSink;
use openalgo_desktop_lib::agent::safety::audit::{self, Filter, Listing};
use openalgo_desktop_lib::agent::safety::kill_switch;
use openalgo_desktop_lib::agent::tools::orders;
use openalgo_desktop_lib::brokers::mock::{AfterSend, MockCall};
use openalgo_desktop_lib::security::Secret;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The trader's Agent settings, as rows (the config part owns the table;
/// it is created here when that part is not in this build).
fn settings(h: &H, pairs: &[(&str, &str)]) {
    let c = h.ctx.sqlite.conn().unwrap();
    c.execute_batch("CREATE TABLE IF NOT EXISTS ag_setting (key TEXT PRIMARY KEY, value TEXT)")
        .unwrap();
    for (k, v) in pairs {
        c.execute(
            "INSERT INTO ag_setting (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [k, v],
        )
        .unwrap();
    }
}

fn trading(h: &H) {
    settings(h, &[("trading_enabled", "true")]);
}

fn run(h: &H, session: &str) -> ToolCtx {
    ToolCtx {
        app: h.ctx.clone(),
        surface: Surface::Chat,
        conversation_id: Some(1),
        run_id: format!("run-{}", session),
        session_id: session.into(),
        user_id: "trader".into(),
        api_key: Secret::new(h.key.clone()),
        caps: Capabilities {
            trading_enabled: true,
            ..Default::default()
        },
        analyzer_mode: false,
        operator_message: String::new(),
        chart_context: None,
        viz: VizSink::new(),
        cancel: CancellationToken::new(),
    }
}

fn tool(ctx: &ToolCtx, name: &str) -> Arc<dyn AgentTool> {
    (orders::GROUP.build)(ctx)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

/// What the model reads: the JSON inside the `<tool_result>` block.
async fn call(ctx: &ToolCtx, name: &str, args: Value) -> Value {
    match tool(ctx, name).call(ctx, args.as_object().cloned().unwrap()).await {
        ToolOutcome::Ok(text) => {
            let start = text.find('\n').unwrap();
            let end = text.rfind('\n').unwrap();
            serde_json::from_str(&text[start + 1..end]).unwrap()
        }
        ToolOutcome::Retry(text) => json!({"retry": text}),
    }
}

fn buy(symbol: &str) -> Value {
    json!({"symbol": symbol, "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS"})
}

fn placed(h: &H) -> usize {
    h.mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::PlaceOrder(_)))
        .count()
}

async fn trail(h: &H) -> Vec<Value> {
    let f = Filter {
        limit: 500,
        ..Filter::default()
    };
    match audit::list(&h.ctx, "trader", f).await.unwrap() {
        Listing::Rows(r) => r,
        Listing::NotYours => panic!(),
    }
}

/// X-037: with analyzer mode on, an approved order goes to the sandbox and
/// the reply says so; nothing reaches the broker. The audit pair is
/// written and linked, and no credential reaches either row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_order_in_analyzer_mode_goes_to_the_sandbox() {
    let h = H::new().await;
    trading(&h);
    h.analyze(true);
    let out = call(&run(&h, "s1"), "place_order", buy("SBIN")).await;
    assert_eq!(out["ok"], true, "{}", out);
    assert_eq!(out["status"], "success");
    assert_eq!(out["response"]["mode"], "analyze", "{}", out);
    assert_eq!(out["order"]["strategy"], "OpenAlgo Agent");
    assert_eq!(out["risk"]["code"], "ok");
    let ids = out["order_ids"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 1, "{}", out);
    assert_eq!(placed(&h), 0, "nothing reached the broker");
    let rows = trail(&h).await;
    let (result, attempt) = (&rows[0], &rows[1]);
    assert_eq!((attempt["phase"].as_str(), result["phase"].as_str()), (Some("attempt"), Some("result")));
    assert_eq!(result["attempt_id"], attempt["id"]);
    assert_eq!(result["ok"], true);
    assert_eq!(result["risk_verdict"], "ok");
    assert_eq!(result["order_ids"], json!(ids));
    assert_eq!(attempt["args"]["symbol"], "SBIN");
    assert!(!serde_json::to_string(&rows).unwrap().contains(&h.key));
    h.shutdown().await;
}

/// X-035, X-036 (decision 14): in Semi-Auto an approved new order waits in
/// the Action Center, and modify, cancel, cancel all and close all are
/// refused with the platform's own sentence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semi_auto_queues_new_orders_and_refuses_the_rest() {
    let h = H::new().await;
    settings(&h, &[("trading_enabled", "true"), ("allow_bulk_destructive", "true")]);
    h.analyze(false);
    ApiKeyService::set_order_mode(&h.ctx, "semi_auto").unwrap();
    let ctx = run(&h, "semi");
    let out = call(&ctx, "place_order", buy("SBIN")).await;
    assert_eq!(out["ok"], true, "{}", out);
    assert_eq!(out["response"]["message"], "Order queued for approval in Action Center");
    assert_eq!(out["response"]["mode"], "semi_auto");
    assert!(out["response"]["pending_order_id"].is_i64(), "{}", out);
    assert_eq!(out["order_ids"], json!([]));
    assert_eq!(placed(&h), 0);
    let modify = json!({"order_id": "A1", "symbol": "SBIN", "exchange": "NSE", "action": "BUY",
        "quantity": 1, "product": "MIS", "price_type": "LIMIT", "price": 100.0});
    for (name, args, sentence) in [
        ("modify_order", modify, "Modify order operation is not allowed in Semi-Auto mode. Please switch to Auto mode to modify orders"),
        ("cancel_order", json!({"order_id": "A1"}), "Cancel order operation is not allowed in Semi-Auto mode. Please switch to Auto mode to cancel orders"),
        ("cancel_all_orders", json!({}), "Cancel all orders operation is not allowed in Semi-Auto mode. Please switch to Auto mode to cancel orders"),
        ("close_all_positions", json!({}), "Close position operation is not allowed in Semi-Auto mode. Please switch to Auto mode to close positions"),
    ] {
        let out = call(&ctx, name, args).await;
        assert_eq!((out["ok"].as_bool(), out["retry"].as_bool()), (Some(false), Some(false)), "{}: {}", name, out);
        let message = out["message"].as_str().unwrap();
        assert!(message.contains(sentence), "{}: {}", name, message);
        assert!(message.starts_with("The broker or the platform refused this request"), "{}", message);
    }
    h.shutdown().await;
}

/// Q-548: a refusal from the broker is reported, not retried, and gives
/// the claim back: the same order can be sent again at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_refusal_gives_the_claim_back_and_says_why() {
    let h = H::new().await;
    trading(&h);
    h.analyze(false);
    h.mock.order_ids.lock().push_back(Err("Invalid quantity".into()));
    let ctx = run(&h, "refused");
    let out = call(&ctx, "place_order", buy("SBIN")).await;
    assert_eq!(out["ok"], false, "{}", out);
    assert_eq!(out["status"], "error");
    assert_eq!(out["retry"], false);
    let message = out["message"].as_str().unwrap();
    assert!(message.contains("Invalid quantity"), "{}", message);
    assert!(!message.contains("call the tool again") && !message.contains("HTTP"), "{}", message);
    let again = call(&ctx, "place_order", buy("SBIN")).await;
    assert_eq!(again["ok"], true, "the claim was given back: {}", again);
    h.shutdown().await;
}

/// Q-735, Q-549: an order whose answer was lost after it reached the
/// broker is reported as unknown, never as failed or safe to resend, and
/// its claim is kept, so the identical order sent again at once is refused
/// as a duplicate instead of being placed twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_answer_is_unknown_and_never_sent_twice() {
    let h = H::new().await;
    trading(&h);
    h.analyze(false);
    h.mock.after_send.lock().push_back((AfterSend::Dropped, "open"));
    let ctx = run(&h, "lost");
    let out = call(&ctx, "place_order", buy("SBIN")).await;
    assert_eq!(out["status"], "unknown", "{}", out);
    assert_eq!(out["retry"], false);
    assert!(out["message"].as_str().unwrap().contains("do NOT send it again"));
    let again = call(&ctx, "place_order", buy("SBIN")).await;
    assert_eq!(again["status"], "blocked", "{}", again);
    assert_eq!(again["code"], "duplicate_order");
    assert_eq!(placed(&h), 1, "the order reached the broker once");
    let rows = trail(&h).await;
    let unknown = rows.iter().find(|r| r["response"]["status"] == "unknown").unwrap();
    assert!(unknown["response"]["exception"].as_str().unwrap().starts_with("Uncertain"), "{}", unknown);
    h.shutdown().await;
}

/// D-003 (Q-1036): the session order cap and the duplicate window survive
/// a restart; the web forgot both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_cap_and_duplicate_window_survive_a_restart() {
    let h = H::new().await;
    settings(&h, &[("trading_enabled", "true"), ("max_orders_per_session", "1")]);
    h.analyze(true);
    assert_eq!(call(&run(&h, "capped"), "place_order", buy("SBIN")).await["ok"], true);
    assert_eq!(call(&run(&h, "window"), "place_order", buy("INFY")).await["ok"], true);
    let h = h.restart().await;
    h.analyze(true);
    let capped = call(&run(&h, "capped"), "place_order", buy("TCS")).await;
    assert_eq!(capped["code"], "session_cap_reached", "{}", capped);
    let repeated = call(&run(&h, "window"), "place_order", buy("INFY")).await;
    assert_eq!(repeated["code"], "duplicate_order", "{}", repeated);
    h.shutdown().await;
}

/// The kill switch, both halves, on and off; it stops cancels too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_kill_switch_stops_every_order_tool_until_released() {
    let h = H::new().await;
    trading(&h);
    h.analyze(true);
    let ctx = run(&h, "kill");
    let path = kill_switch::set_for(&h.ctx, true).unwrap();
    assert_eq!(path, h.ctx.data_dir.join("agent_kill_switch"));
    let out = call(&ctx, "place_order", buy("SBIN")).await;
    assert_eq!(out["code"], "kill_switch", "{}", out);
    assert_eq!(out["risk"]["details"]["source"], "file");
    assert_eq!(call(&ctx, "cancel_order", json!({"order_id": "A1"})).await["code"], "kill_switch");
    kill_switch::set_for(&h.ctx, false).unwrap();
    assert_eq!(call(&ctx, "place_order", buy("SBIN")).await["ok"], true);
    settings(&h, &[("kill_switch", "true")]);
    let out = call(&ctx, "place_order", buy("TCS")).await;
    assert_eq!(out["risk"]["details"]["source"], "setting", "{}", out);
    settings(&h, &[("kill_switch", "false")]);
    assert_eq!(call(&ctx, "place_order", buy("TCS")).await["ok"], true);
    h.shutdown().await;
}

/// X-004, X-005, X-023: the trading switch turned off after the approval,
/// analyzer-only in live mode, and an account-wide operation while those
/// are off, each refused by the guard inside the tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trading_switch_analyzer_only_and_bulk_permission_are_enforced() {
    let h = H::new().await;
    h.analyze(false);
    let ctx = run(&h, "switches");
    assert_eq!(call(&ctx, "place_order", buy("SBIN")).await["code"], "trading_disabled");
    settings(&h, &[("trading_enabled", "true"), ("require_analyzer_mode", "true")]);
    assert_eq!(call(&ctx, "place_order", buy("SBIN")).await["code"], "analyzer_required");
    h.analyze(true);
    assert_eq!(call(&ctx, "place_order", buy("SBIN")).await["ok"], true);
    let bulk = call(&ctx, "cancel_all_orders", json!({})).await;
    assert_eq!(bulk["code"], "bulk_operation_not_allowed", "{}", bulk);
    assert_eq!(call(&ctx, "close_all_positions", json!({})).await["code"], "bulk_operation_not_allowed");
    settings(&h, &[("allow_bulk_destructive", "true")]);
    let allowed = call(&ctx, "cancel_all_orders", json!({})).await;
    assert_eq!(allowed["ok"], true, "{}", allowed);
    h.shutdown().await;
}

/// The size, value and price limits through the tools, and D-007: a
/// modify meets them too (the web let a modify past them).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quantity_value_and_price_limits_hold_for_new_and_modified_orders() {
    let h = H::new().await;
    settings(&h, &[("trading_enabled", "true"), ("max_order_quantity", "100"), ("max_order_value", "1000")]);
    h.analyze(true);
    let ctx = run(&h, "limits");
    let big = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 101, "product": "MIS"});
    assert_eq!(call(&ctx, "place_order", big).await["code"], "quantity_exceeded");
    let dear = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 100, "product": "MIS"});
    assert_eq!(call(&ctx, "place_order", dear).await["code"], "notional_exceeded");
    settings(&h, &[("max_order_value", "100000000")]);
    let far = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS",
        "price_type": "LIMIT", "price": 1.0});
    assert_eq!(call(&ctx, "place_order", far).await["code"], "price_deviation");
    let modify = json!({"order_id": "A1", "symbol": "SBIN", "exchange": "NSE", "action": "BUY",
        "quantity": 101, "product": "MIS", "price_type": "MARKET"});
    let out = call(&ctx, "modify_order", modify).await;
    assert_eq!(out["code"], "quantity_exceeded", "{}", out);
    h.shutdown().await;
}

/// T-162: a fixable argument is a correction the model reads; nothing is
/// checked or sent, and the attempt and its validation result are on
/// record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fixable_argument_is_a_correction_with_its_audit_pair() {
    let h = H::new().await;
    trading(&h);
    h.analyze(false);
    let mut args = buy("SBIN");
    args["price"] = json!(1450.5);
    let out = call(&run(&h, "fix"), "place_order", args).await;
    let text = out["retry"].as_str().unwrap();
    assert!(text.starts_with("The 'price' argument is invalid: a MARKET order carries no limit price"), "{}", text);
    assert_eq!(placed(&h), 0);
    let rows = trail(&h).await;
    assert_eq!(rows[0]["response"]["stage"], "validation");
    assert_eq!(rows[0]["attempt_id"], rows[1]["id"]);
    h.shutdown().await;
}

/// Q-546, Q-547: a square-off that reads zero sends nothing and says that
/// zero is not proof the position is closed; one with a held quantity
/// sends the platform's square-off (a smart order to zero).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_position_reads_it_first() {
    let h = H::new().await;
    trading(&h);
    h.analyze(false);
    let ctx = run(&h, "close");
    let out = call(&ctx, "close_position", json!({"symbol": "NSE:SBIN", "exchange": "", "product": "intraday"})).await;
    assert_eq!(out["status"], "nothing_to_close", "{}", out);
    assert_eq!((out["symbol"].as_str(), out["exchange"].as_str(), out["product"].as_str()), (Some("SBIN"), Some("NSE"), Some("MIS")));
    assert!(out["message"].as_str().unwrap().contains("does not prove the position is closed"));
    assert_eq!(placed(&h), 0);
    h.mock.positions.lock().replace(Ok(vec![openalgo_desktop_lib::brokers::types::Position {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: 50,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 100.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: 50,
        buy_value: 5000.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }]));
    let out = call(&ctx, "close_position", json!({"symbol": "SBIN", "exchange": "NSE", "product": "MIS"})).await;
    assert_eq!(out["ok"], true, "{}", out);
    assert_eq!(placed(&h), 1, "the square-off was sent");
    h.shutdown().await;
}
