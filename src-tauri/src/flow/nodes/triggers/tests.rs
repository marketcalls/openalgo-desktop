//! The priceAlert node against the web's `execute_price_alert` (goldens
//! `tests/fixtures/web/flow/goldens/triggers/priceAlert__*.json`), and the
//! web defects the desktop fixes, each with the web's behaviour refused.

use super::*;
use crate::flow::contract::client::fake::FakeFlowClient;

const QUOTE: &str =
    r#"{"status": "success", "data": {"ltp": 812.5, "open": 806.0, "prev_close": 800.0}}"#;

struct Run {
    out: NodeOutcome,
    logs: Vec<(String, String)>,
    ctx: NodeCtx,
    client: Arc<FakeFlowClient>,
}

async fn run_with(data: Value, vars: Value, trigger: RunTrigger) -> Run {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("quotes", serde_json::from_str(QUOTE).unwrap());
    let ctx = NodeCtx::bare(client.clone()).with_trigger(trigger);
    for (k, v) in vars.as_object().unwrap() {
        ctx.vars.set(k, v.clone());
    }
    let out = PriceAlert.run(&ctx, data.as_object().unwrap()).await;
    let logs = ctx
        .logs()
        .iter()
        .map(|l| {
            (
                l["level"].as_str().unwrap().to_string(),
                l["message"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    Run {
        out,
        logs,
        ctx,
        client,
    }
}

async fn run(data: Value) -> Run {
    run_with(data, json!({}), RunTrigger::Manual).await
}

fn alert(condition: &str, price: Value) -> Value {
    json!({"symbol": "SBIN", "exchange": "NSE", "condition": condition, "price": price, "outputVariable": "alert"})
}

/// parity N-priceAlert-01, -06, -08: the editor's words and the canonical
/// ones, the level tests on one quote, the log line and the stored output.
#[tokio::test]
async fn each_condition_is_tested_on_one_quote_as_the_web_does() {
    for (condition, price, extra, met) in [
        ("above", json!(800), json!({}), true),
        ("above", json!(900), json!({}), false),
        ("greater_than", json!(800), json!({}), true),
        ("below", json!(900), json!({}), true),
        ("crosses_above", json!(800), json!({}), true),
        ("Crosses-Below", json!(900), json!({}), true),
        ("crossing", json!(812.0), json!({}), true),
        ("crosses", json!(800.0), json!({}), false),
        (
            "inside_channel",
            json!(800),
            json!({"priceLower": 800, "priceUpper": "{{hi}}"}),
            true,
        ),
        (
            "entering_channel",
            json!(800),
            json!({"priceLower": 820, "priceUpper": 830}),
            false,
        ),
        (
            "outside_channel",
            json!(800),
            json!({"priceLower": 820, "priceUpper": 830}),
            true,
        ),
        (
            "exiting_channel",
            json!(800),
            json!({"priceLower": 800, "priceUpper": 830}),
            false,
        ),
    ] {
        let mut data = alert(condition, price);
        for (k, v) in extra.as_object().unwrap() {
            data[k] = v.clone();
        }
        let r = run_with(data, json!({"hi": 815}), RunTrigger::Manual).await;
        assert_eq!(
            r.out.value(),
            Some(&json!({"status": "success", "condition": met, "ltp": 812.5})),
            "{}",
            condition
        );
        assert_eq!(
            r.ctx.vars.get("alert"),
            Some(json!({"ltp": 812.5, "condition_met": met}))
        );
        assert!(
            r.logs[0].1.starts_with("Price alert: SBIN LTP=812.5 "),
            "{:?}",
            r.logs
        );
        assert_eq!(r.logs[1].1, "Stored result in variable: alert");
    }
    let r = run(alert("above", json!(800))).await;
    assert_eq!(
        r.logs[0].1,
        "Price alert: SBIN LTP=812.5 greater_than 800.0 = True"
    );
    let r = run(alert("crosses_above", json!("{{lvl}}"))).await;
    assert!(
        r.out.is_error(),
        "an unresolved level is refused, not read as 0"
    );
    let r = run_with(
        json!({"symbol": "{{webhook.symbol}}", "condition": null, "price": "{{lvl}}"}),
        json!({"webhook": {"symbol": "SBIN"}, "lvl": "810"}),
        RunTrigger::Manual,
    )
    .await;
    assert_eq!(
        r.logs[0].1,
        "Price alert: SBIN LTP=812.5 greater_than 810.0 = True"
    );
    assert_eq!(r.logs.len(), 1, "no outputVariable, nothing stored");
    assert_eq!(
        Value::Object(r.client.calls()[0].payload.clone()),
        json!({"symbol": "SBIN", "exchange": "NSE"})
    );
}

/// parity N-priceAlert-02: no symbol is an undecided failure with no
/// message, as on the web.
#[tokio::test]
async fn no_symbol_is_a_failure_without_a_quote() {
    let r = run(alert("above", json!(800))
        .as_object()
        .map(|m| {
            let mut m = m.clone();
            m.insert("symbol".into(), json!(""));
            Value::Object(m)
        })
        .unwrap())
    .await;
    assert_eq!(
        r.out.value(),
        Some(&json!({"status": "error", "condition": false}))
    );
    assert!(r.client.calls().is_empty());
    assert!(r.logs.is_empty());
}

/// parity N-priceAlert-03, N-priceAlert-04 (WEB-DEFECT): a run the price
/// monitor started trusts its tick; a webhook body that says it is one is
/// checked against a quote like any other run. On the web the second call
/// took the true branch at LTP 1.0 with no quote.
#[tokio::test]
async fn only_the_price_monitor_skips_the_quote() {
    let claim = json!({"webhook": {"trigger_type": "price_alert", "trigger_price": 1}});
    let r = run_with(
        alert("above", json!(900)),
        claim.clone(),
        RunTrigger::Webhook,
    )
    .await;
    assert_eq!(
        r.out.value(),
        Some(&json!({"status": "success", "condition": false, "ltp": 812.5}))
    );
    assert_eq!(r.client.calls().len(), 1, "the price is checked");
    assert_eq!(
        r.logs[0].1,
        "Price alert: SBIN LTP=812.5 greater_than 900.0 = False"
    );

    let fired = json!({"webhook": {"trigger_type": "price_alert", "trigger_price": 812.75}});
    let r = run_with(alert("above", json!(800)), fired, RunTrigger::PriceAlert).await;
    assert_eq!(
        r.out.value(),
        Some(&json!({"status": "success", "condition": true, "ltp": 812.75}))
    );
    assert!(r.client.calls().is_empty());
    assert_eq!(
        r.logs[0].1,
        "Price alert fired by monitor: SBIN LTP=812.75 greater_than 800.0"
    );
    assert_eq!(
        r.ctx.vars.get("alert"),
        Some(json!({"ltp": 812.75, "condition_met": true}))
    );
}

/// parity N-priceAlert-05 (WEB-DEFECT): a failed quote carries the
/// client's reason; the web dropped it and the run read `node failed`.
#[tokio::test]
async fn a_failed_quote_says_why() {
    let client = Arc::new(FakeFlowClient::new());
    client.reply_error("quotes", "Invalid openalgo apikey", 403);
    let ctx = NodeCtx::bare(client);
    let out = PriceAlert
        .run(&ctx, alert("above", json!(800)).as_object().unwrap())
        .await;
    assert_eq!(
        out.value(),
        Some(&json!({"status": "error", "condition": false, "message": "Invalid openalgo apikey"}))
    );
    assert_eq!(out.failure_message(), "Invalid openalgo apikey");
}

/// parity N-priceAlert-07 (WEB-DEFECT): the previous-tick conditions and an
/// unknown one log the web's sentence and carry it as `message`.
#[tokio::test]
async fn a_condition_the_node_cannot_test_says_why() {
    let r = run(alert("moving_up", json!(800))).await;
    let sentence = "Price alert condition 'moving_up' needs the previous tick and can only be evaluated by the price monitor; wire this node as the trigger.";
    assert_eq!(
        r.out.value(),
        Some(&json!({"status": "error", "condition": false, "ltp": 812.5, "message": sentence}))
    );
    assert_eq!(r.logs, vec![("error".to_string(), sentence.to_string())]);
    let r = run(alert("near", json!(800))).await;
    assert_eq!(
        r.out.failure_message(),
        "Price alert has an unrecognized condition 'near'; it can never be true."
    );
}

/// parity N-priceAlert-09 (WEB-DEFECT): a level that is given but does not
/// resolve fails closed. The web read it as 0, so `above {{level}}` was
/// true on any positive price and took the true branch.
#[tokio::test]
async fn an_unresolved_level_fails_closed() {
    let r = run(alert("above", json!("{{level}}"))).await;
    assert!(r.out.is_error());
    assert_eq!(r.out.condition_met(), Some(false));
    assert!(r.client.calls().is_empty(), "nothing is quoted");
    assert_eq!(r.ctx.vars.get("alert"), None);
    assert_eq!(
        r.out.failure_message(),
        "Price alert price '{{level}}' is not a number, so the alert was not evaluated."
    );
    // A channel bound only matters to a channel condition.
    let mut data = alert("above", json!(800));
    data["priceUpper"] = json!("{{nope}}");
    assert!(!run(data.clone()).await.out.is_error());
    data["condition"] = json!("inside_channel");
    assert!(run(data).await.out.is_error());
    // Absent stays the web's 0.
    let r = run(json!({"symbol": "SBIN"})).await;
    assert_eq!(
        r.logs[0].1,
        "Price alert: SBIN LTP=812.5 greater_than 0 = True"
    );
}
