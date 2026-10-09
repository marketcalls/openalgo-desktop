//! web: test/test_strategy_module_webhook.py

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
