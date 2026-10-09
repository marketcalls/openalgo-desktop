//! force_live and per-run destinations against a full app context.

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
