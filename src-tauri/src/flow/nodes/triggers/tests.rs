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
    run_quoted(data, vars, trigger, serde_json::from_str(QUOTE).unwrap()).await
}

/// One run of the node answering its quote with `quote`.
async fn run_quoted(data: Value, vars: Value, trigger: RunTrigger, quote: Value) -> Run {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("quotes", quote);
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

/// parity N-priceAlert-02 (WEB-DEFECT): no symbol is an undecided failure
/// before any quote, saying what is missing (the web gave no reason, so the
/// run read only "node failed").
#[tokio::test]
async fn no_symbol_is_a_failure_without_a_quote() {
    let mut data = alert("above", json!(800));
    data["symbol"] = json!("");
    let r = run(data).await;
    assert_eq!(
        r.out.value(),
        Some(&json!({"status": "error", "condition": false,
            "message": "A priceAlert node needs symbol, so the alert was not evaluated."}))
    );
    assert_eq!(r.out.failure_message(), NO_SYMBOL);
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

/// parity N-orderUpdateTrigger-00, -01: every trigger-lane node type is in
/// the registry the executor builds (a type missing from it fails the run
/// as unknown); the order-update trigger node writes its line and lets its
/// children run.
#[tokio::test]
async fn the_trigger_nodes_are_in_the_executors_registry() {
    let registry = crate::flow::nodes::NodeRegistry::default();
    for kind in [
        "start",
        "webhookTrigger",
        "priceAlert",
        "orderUpdateTrigger",
        "subscribeLtp",
        "subscribeQuote",
        "subscribeDepth",
        "unsubscribe",
    ] {
        assert!(registry.get(kind).is_some(), "{} is not registered", kind);
    }
    let node = registry.get("orderUpdateTrigger").unwrap();
    let ctx = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    let out = node.run(&ctx, &Map::new()).await;
    assert!(!out.is_error());
    let lines: Vec<Value> = ctx.logs().iter().map(|l| l["message"].clone()).collect();
    assert_eq!(lines, vec![json!("Order-update trigger activated")]);
}

/// One run of the node on `quote` instead of the usual SBIN quote.
async fn run_on(data: Value, quote: Value) -> Run {
    run_quoted(data, json!({}), RunTrigger::Manual, quote).await
}

fn ltp(v: f64) -> Value {
    json!({"status": "success", "data": {"ltp": v}})
}

/// parity N-priceAlert-01 (decisions.md 24, 36): a list or dict price or
/// bound ends the whole run with Python's text, as the web's get_float
/// raised: before the symbol is checked, before any quote, for a bound the
/// condition does not even read.
#[tokio::test]
async fn a_list_or_dict_level_ends_the_run() {
    let float_err = |t: &str| {
        format!(
            "float() argument must be a string or a real number, not '{}'",
            t
        )
    };
    for (key, value, kind) in [
        ("price", json!([800]), "list"),
        ("price", json!({"v": 800}), "dict"),
        ("priceLower", json!([790]), "list"),
        ("priceUpper", json!({}), "dict"),
    ] {
        let mut data = alert("above", json!(800));
        data[key] = value;
        let r = run(data.clone()).await;
        assert_eq!(r.out.aborted(), Some(float_err(kind).as_str()), "{}", key);
        assert!(r.client.calls().is_empty(), "{}: nothing is quoted", key);
        assert!(r.logs.is_empty(), "{:?}", r.logs);
        data["symbol"] = json!("");
        assert_eq!(
            run(data).await.out.aborted(),
            Some(float_err(kind).as_str()),
            "{}: the level is read before the symbol",
            key
        );
    }
    // The web reads price first.
    let mut data = alert("above", json!([1]));
    data["priceLower"] = json!({});
    assert_eq!(
        run(data).await.out.aborted(),
        Some(float_err("list").as_str())
    );
    // A number, text or bool is no abort.
    let mut data = alert("above", json!(true));
    data["priceUpper"] = json!("900");
    assert_eq!(run(data).await.out.aborted(), None);
}

/// parity N-priceAlert-05: a failed quote with no text is the web's bare
/// `{status error, condition false}`; a successful quote with no data or
/// no ltp reads ltp 0 and is evaluated (the web's `float(... or 0)`).
#[tokio::test]
async fn a_quote_without_text_or_ltp_reads_as_the_web() {
    for reply in [
        json!({"status": "error"}),
        json!({"status": "error", "message": "", "error": null}),
        json!({"status": "failed", "data": {"ltp": 900}}),
    ] {
        let r = run_on(alert("above", json!(800)), reply.clone()).await;
        assert_eq!(
            r.out.value(),
            Some(&json!({"status": "error", "condition": false})),
            "{}",
            reply
        );
        assert!(r.logs.is_empty());
        assert_eq!(r.ctx.vars.get("alert"), None);
    }
    for reply in [
        json!({"status": "success"}),
        json!({"status": "success", "data": null}),
        json!({"status": "success", "data": {}}),
        json!({"status": "success", "data": {"bid": 812.4}}),
    ] {
        let r = run_on(alert("below", json!(800)), reply.clone()).await;
        assert_eq!(
            r.out.value(),
            Some(&json!({"status": "success", "condition": true, "ltp": 0.0})),
            "{}",
            reply
        );
        assert_eq!(
            r.logs[0].1,
            "Price alert: SBIN LTP=0.0 less_than 800.0 = True"
        );
        assert_eq!(
            r.ctx.vars.get("alert"),
            Some(json!({"ltp": 0.0, "condition_met": true}))
        );
    }
}

/// parity N-priceAlert-06: the node's crossing tolerance is the level's
/// 0.1 % (`price * 0.001`), inclusive: at 1000 the band is exactly 999 to
/// 1001. A smaller factor misses the edges, a larger one or the LTP's 0.1 %
/// (the monitor's) takes 1001.0005.
#[tokio::test]
async fn the_crossing_band_is_the_levels_tenth_of_a_percent() {
    for (at, met) in [
        (1001.0, true),
        (999.0, true),
        (1000.0, true),
        (1001.0005, false),
        (1001.001, false),
        (998.999, false),
    ] {
        let r = run_on(alert("crossing", json!(1000)), ltp(at)).await;
        assert_eq!(r.out.condition_met(), Some(met), "LTP {}", at);
        assert!(!r.out.is_error());
    }
}

/// parity N-priceAlert-07: the unknown-condition sentence quotes the
/// condition as stored, not its canonical word, and a number as a number.
#[tokio::test]
async fn an_unknown_condition_is_quoted_as_stored() {
    let r = run(alert("Near-By", json!(800))).await;
    let sentence = "Price alert has an unrecognized condition 'Near-By'; it can never be true.";
    assert_eq!(
        r.out.value(),
        Some(&json!({"status": "error", "condition": false, "ltp": 812.5, "message": sentence}))
    );
    assert_eq!(r.logs, vec![("error".to_string(), sentence.to_string())]);
    let mut data = alert("", json!(800));
    data["condition"] = json!(7);
    assert_eq!(
        run(data).await.out.failure_message(),
        "Price alert has an unrecognized condition 7; it can never be true."
    );
}

/// parity N-priceAlert-09: an unresolved bound fails closed under every
/// channel condition, the lower one included, and under no other.
#[tokio::test]
async fn an_unresolved_bound_fails_closed_under_every_channel_condition() {
    for condition in [
        "inside_channel",
        "entering_channel",
        "exiting_channel",
        "outside_channel",
    ] {
        for (key, name) in [("priceLower", "lower price"), ("priceUpper", "upper price")] {
            let mut data = alert(condition, json!(800));
            data["priceLower"] = json!(790);
            data["priceUpper"] = json!(820);
            data[key] = json!("{{band}}");
            let r = run(data).await;
            assert_eq!(
                r.out.failure_message(),
                format!(
                    "Price alert {} '{{{{band}}}}' is not a number, so the alert was not evaluated.",
                    name
                ),
                "{} {}",
                condition,
                key
            );
            assert_eq!(r.out.condition_met(), Some(false));
            assert!(r.client.calls().is_empty(), "{} {}", condition, key);
            assert_eq!(r.logs[0].0, "error");
        }
    }
    for condition in [
        "above",
        "below",
        "crossing",
        "crosses_above",
        "crosses_below",
    ] {
        let mut data = alert(condition, json!(800));
        data["priceLower"] = json!("{{band}}");
        assert!(!run(data).await.out.is_error(), "{}", condition);
    }
}
