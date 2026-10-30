//! The web's tests of the data nodes, ported (`test_flow_default_product.py`,
//! `test_flow_final_fix.py` margin cases, `test_flow_qa_regressions.py`
//! option-expiry and params cases), and one test per web defect this lane
//! fixes, each failing on the web's behaviour.

use super::support::{ctx, ctx_at, logs, run};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::NodeEnv;
use crate::flow::contract::model::Graph;
use crate::flow::executor::walk::Walk;
use crate::flow::nodes::history::shared_cache;
use crate::flow::nodes::{data, NodeRegistry};
use serde_json::{json, Value};
use std::sync::Arc;

fn ok_margin() -> Value {
    json!({"status": "success", "data": {"total_margin_required": 1.0, "span_margin": 0.0, "exposure_margin": 0.0}})
}

fn candles(n: i64) -> Value {
    let bars: Vec<Value> = (0..n)
        .map(|i| json!({"timestamp": 1_780_000_000 + i * 86_400, "open": 100.0, "high": 101.0 + i as f64, "low": 99.0, "close": 100.0 + i as f64, "volume": 1000}))
        .collect();
    json!({"status": "success", "data": bars})
}

/// test_flow_final_fix.py:570-600 (X-092): a raw list or object template reaches
/// margin as the service's string-valued shape.
#[tokio::test]
async fn margin_resolves_structured_templates_into_validated_positions() {
    let leg = json!({"symbol": " SBIN ", "exchange": " nse ", "action": " buy ", "quantity": 2,
                     "product": " mis ", "pricetype": " limit ", "price": 100.5});
    for runtime in [json!([leg.clone()]), leg.clone()] {
        let client = Arc::new(FakeFlowClient::new());
        client.reply("margin", ok_margin());
        let c = ctx(client.clone());
        c.vars.set("runtime", runtime);
        let out = run(&c, "margin", json!({"positionsJson": "{{runtime}}"})).await;
        assert_eq!(
            out.value().map(|v| v["status"].clone()),
            Some(json!("success"))
        );
        let calls = client.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].payload["positions"],
            json!([{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": "2",
                    "product": "MIS", "pricetype": "LIMIT", "price": "100.5"}])
        );
    }
}

/// test_flow_final_fix.py:603-676 (X-093): every resolved leg is checked before
/// any call.
#[tokio::test]
async fn margin_refuses_invalid_positions_before_the_client() {
    let base = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1,
                      "product": "MIS", "pricetype": "MARKET"});
    let with = |k: &str, v: Value| {
        let mut p = base.clone();
        p[k] = v;
        json!([p])
    };
    let cases = [
        (json!([]), "empty"),
        (json!([{"symbol": "SBIN"}]), "exchange"),
        (with("quantity", json!("{{missing.qty}}")), "quantity"),
        (with("exchange", json!("BAD")), "exchange"),
        (with("quantity", json!(true)), "quantity"),
        (
            json!([{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1,
                    "product": "MIS", "pricetype": "LIMIT", "price": "inf"}]),
            "price",
        ),
    ];
    for (positions, field) in cases {
        let client = Arc::new(FakeFlowClient::new());
        let c = ctx(client.clone());
        c.vars.set("runtime", positions.clone());
        let out = run(&c, "margin", json!({"positions": "{{runtime}}"})).await;
        let v = out.value().cloned().unwrap_or(Value::Null);
        assert_eq!(v["status"], "error", "{}", positions);
        let message = v["message"].as_str().unwrap_or_default().to_lowercase();
        assert!(message.contains(field), "{} -> {}", positions, message);
        assert!(client.calls().is_empty());
    }
}

/// N-margin-06 (web defect): a basket the editor emptied (`""`) is absent,
/// so the node prices its single position. The web failed every run with
/// "Margin positions is empty after variable interpolation".
#[tokio::test]
async fn a_blank_basket_prices_the_single_position() {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("margin", ok_margin());
    let c = ctx(client.clone());
    let out = run(
        &c,
        "margin",
        json!({"positionsJson": "", "symbol": "SBIN", "exchange": "NSE", "quantity": 10,
               "price": 812.5, "priceType": "LIMIT", "product": "MIS"}),
    )
    .await;
    assert!(!out.is_error(), "{:?}", out);
    assert_eq!(
        client.calls()[0].payload["positions"],
        json!([{"exchange": "NSE", "symbol": "SBIN", "action": "BUY", "quantity": "10",
                "product": "MIS", "pricetype": "LIMIT", "price": "812.5"}])
    );
    // A basket that interpolates to blank is still refused.
    let c = ctx(Arc::new(FakeFlowClient::new()));
    c.vars.set("blank", json!("  "));
    let out = run(
        &c,
        "margin",
        json!({"positionsJson": "{{blank}}", "symbol": "SBIN"}),
    )
    .await;
    assert!(out.is_error());
}

/// test_flow_default_product.py:234-237 (X-068) and :245-251 (X-070): a derivatives
/// segment defaults to NRML.
#[tokio::test]
async fn derivatives_default_to_nrml() {
    let client = Arc::new(FakeFlowClient::new());
    client.reply(
        "positionbook",
        json!({"status": "success", "data": [
            {"symbol": "GOLDM", "exchange": "MCX", "product": "MIS", "quantity": 2, "pnl": 1.0},
            {"symbol": "GOLDM", "exchange": "MCX", "product": "NRML", "quantity": 1, "pnl": 5.0}
        ]}),
    );
    client.reply("margin", ok_margin());
    let c = ctx(client.clone());
    let out = run(
        &c,
        "openPosition",
        json!({"symbol": "GOLDM", "exchange": "MCX"}),
    )
    .await;
    assert_eq!(
        out.value().cloned(),
        Some(json!({"status": "success", "quantity": 1, "pnl": 5.0}))
    );
    // X-070 (test_margin): NIFTY28AUG26FUT on NFO, no product, sends NRML.
    run(
        &c,
        "margin",
        json!({"symbol": "NIFTY28AUG26FUT", "exchange": "NFO", "quantity": 1}),
    )
    .await;
    assert_eq!(
        client.calls_to("margin")[0].payload["positions"][0],
        json!({"exchange": "NFO", "symbol": "NIFTY28AUG26FUT", "action": "BUY", "quantity": "1",
               "product": "NRML", "pricetype": "MARKET", "price": "0"})
    );
}

/// test_flow_qa_regressions.py:1090-1153: the option nodes refuse an
/// unresolved or invalid expiry before any call, run with a valid one, and
/// fall back to an expiry embedded in the underlying when none is given.
#[tokio::test]
async fn option_nodes_check_their_expiry_before_any_call() {
    let nodes = [
        ("optionSymbol", "optionsymbol"),
        ("optionChain", "optionchain"),
        ("syntheticFuture", "syntheticfuture"),
    ];
    for (node, endpoint) in nodes {
        for webhook in [
            json!({"underlying": "NIFTY", "expiry": ""}),
            json!({"underlying": "NIFTY", "expiry": "not-a-date"}),
            json!({"underlying": "NIFTY"}),
        ] {
            let client = Arc::new(FakeFlowClient::new());
            let c = ctx(client.clone());
            c.vars.set("webhook", webhook.clone());
            let out = run(
                &c,
                node,
                json!({"underlying": "{{webhook.underlying}}", "expiryDate": "{{webhook.expiry}}"}),
            )
            .await;
            assert!(out.is_error(), "{} {}", node, webhook);
            assert!(client.calls().is_empty(), "{} {}", node, webhook);
        }
        let client = Arc::new(FakeFlowClient::new());
        client.reply(endpoint, json!({"status": "success"}));
        let c = ctx(client.clone());
        c.vars.set(
            "webhook",
            json!({"underlying": "NIFTY", "expiry": "27AUG26"}),
        );
        let out = run(
            &c,
            node,
            json!({"underlying": "{{webhook.underlying}}", "expiryDate": "{{webhook.expiry}}"}),
        )
        .await;
        assert!(!out.is_error(), "{}", node);
        assert_eq!(client.calls()[0].endpoint, endpoint);
    }
    for node in ["optionSymbol", "optionChain"] {
        let client = Arc::new(FakeFlowClient::new());
        let c = ctx(client.clone());
        c.vars.set("webhook", json!({"expiry": "not-a-date"}));
        let out = run(
            &c,
            node,
            json!({"underlying": "NIFTY27AUG26", "expiryDate": "{{webhook.expiry}}"}),
        )
        .await;
        assert!(out.is_error() && client.calls().is_empty(), "{}", node);
        let client = Arc::new(FakeFlowClient::new());
        client.reply("optionsymbol", json!({"status": "success"}));
        client.reply("optionchain", json!({"status": "success"}));
        let c = ctx(client.clone());
        let out = run(&c, node, json!({"underlying": "NIFTY27AUG26"})).await;
        assert!(!out.is_error() && client.calls().len() == 1, "{}", node);
    }
}

/// N-optionChain-03 (web defect, decision 10): an explicit expiry wins over
/// one embedded in the underlying. The web's chain service let the
/// embedded one win; the node now asks for the bare underlying.
#[tokio::test]
async fn an_explicit_expiry_wins_for_the_option_chain() {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("optionchain", json!({"status": "success"}));
    let c = ctx(client.clone());
    run(
        &c,
        "optionChain",
        json!({"underlying": "NIFTY25NOV26FUT", "expiryDate": "30DEC26", "strikeCount": 5}),
    )
    .await;
    let p = &client.calls()[0].payload;
    assert_eq!(
        (p["underlying"].clone(), p["expiry_date"].clone()),
        (json!("NIFTY"), json!("30DEC26"))
    );
    assert_eq!(
        logs(&c)[0].1,
        "Fetching option chain for: NIFTY25NOV26FUT expiry=30DEC26"
    );
}

/// test_flow_qa_regressions.py:1946-1970 (X-245): params stored as an
/// object and as JSON text give the same result; the object form warns.
#[tokio::test]
async fn indicator_params_object_and_text_agree() {
    let mut results = Vec::new();
    for params in [json!({"period": 5}), json!("{\"period\": 5}")] {
        shared_cache().clear();
        let client = Arc::new(FakeFlowClient::new());
        client.reply("history", candles(60));
        let c = ctx(client);
        let out = run(
            &c,
            "indicator",
            json!({"symbol": "SBIN", "indicatorName": "sma", "params": params, "lookbackBars": 20}),
        )
        .await;
        results.push(out.value().cloned());
        let warned = logs(&c).iter().any(|(l, _)| l == "warning");
        assert_eq!(warned, params.is_object());
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(
        results[0].as_ref().map(|v| v["latest"]["value"].clone()),
        Some(json!(157.0))
    );
}

/// N-indicator-11 / IND-043 (web defect): a library error that is not a
/// ValueError (an unknown params key) fails only this node; the web aborted
/// the whole run, sibling branches included.
#[tokio::test]
async fn an_indicator_library_error_fails_only_its_node() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", candles(60));
    client.reply("quotes", json!({"status": "success", "data": {"ltp": 1.0}}));
    let c = ctx(client.clone());
    let nodes = json!([
        {"id": "s", "type": "start", "data": {}},
        {"id": "i", "type": "indicator", "data": {"symbol": "SBIN", "indicatorName": "sma", "params": "{\"window\": 5}"}},
        {"id": "q", "type": "getQuote", "data": {"symbol": "INFY"}}
    ]);
    let edges = json!([{"source": "s", "target": "i"}, {"source": "s", "target": "q"}]);
    let graph = Graph::parse(&nodes, &edges).unwrap_or_default();
    let registry = NodeRegistry::new(&[crate::flow::nodes::core::FAMILY, data::FAMILY]);
    let mut walk = Walk::new(&graph, &registry, &c);
    let abort = walk.visit("s", 0).await.err();
    assert_eq!(abort, None, "the run is not aborted");
    assert_eq!(walk.errors.len(), 1);
    assert_eq!(walk.errors[0]["type"], "indicator");
    assert!(logs(&c)
        .iter()
        .any(|(l, m)| l == "error" && m.starts_with("Indicator error: ")));
    assert_eq!(client.calls_to("quotes").len(), 1, "the sibling branch ran");
}

/// N-indicator-03 (web defect): a whitespace-only sourceSeries is absent
/// (as the validator reads it), so the node computes from history; the web
/// entered nested mode and failed "did not resolve to a JSON array".
#[tokio::test]
async fn a_blank_source_series_computes_from_history() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", candles(60));
    let c = ctx(client.clone());
    let out = run(&c, "indicator", json!({"symbol": "SBIN", "indicatorName": "sma", "params": "{\"period\": 3}", "sourceSeries": "  "})).await;
    assert!(!out.is_error(), "{:?}", out);
    assert_eq!(client.calls_to("history").len(), 1);
}

/// Nested mode reads another node's series, a history's candles, plain
/// numbers and JSON text with NaN, as the web's Python reads them.
#[tokio::test]
async fn nested_series_come_from_any_upstream_shape() {
    let c = ctx(Arc::new(FakeFlowClient::new()));
    c.vars.set(
        "rsi",
        json!({"series": [{"value": 1.0}, {"value": 2.0}, {"value": null}, {"value": 3.0}]}),
    );
    c.vars
        .set("h", json!({"data": [{"close": 4.0}, {"close": 6.0}]}));
    let sma = |src: &str| json!({"indicatorName": "sma", "params": "{\"period\": 2}", "sourceSeries": src, "tailBars": 2});
    let latest = |out: crate::flow::contract::node::NodeOutcome| {
        out.value().map(|v| v["latest"]["value"].clone())
    };
    assert_eq!(
        latest(run(&c, "indicator", sma("{{rsi.series}}")).await),
        Some(json!(2.5))
    );
    assert_eq!(
        latest(run(&c, "indicator", sma("{{h.data}}")).await),
        Some(json!(5.0))
    );
    assert_eq!(
        latest(run(&c, "indicator", sma("[1, 2, 3, NaN]")).await),
        Some(Value::Null)
    );
    let refused = run(&c, "indicator", sma("[1, 2,")).await;
    assert_eq!(
        refused.failure_message(),
        "sourceSeries did not resolve to a JSON array"
    );
}

/// N-dataCommon-14 (web defect): the symbol is interpolated once, so a
/// payload value that itself holds `{{...}}` is sent as received; the web
/// resolved it a second time against the run's variables.
#[tokio::test]
async fn a_symbol_is_interpolated_once() {
    for node in ["priorPeriodOhlc", "barOffset", "indicator"] {
        shared_cache().clear();
        let client = Arc::new(FakeFlowClient::new());
        client.reply("history", candles(30));
        let c = ctx(client.clone());
        c.vars.set("webhook", json!({"symbol": "{{secret}}"}));
        c.vars.set("secret", json!("SBIN"));
        run(
            &c,
            node,
            json!({"symbol": "{{webhook.symbol}}", "params": "{\"period\": 3}"}),
        )
        .await;
        assert_eq!(
            client.calls()[0].payload["symbol"],
            "{{secret}}",
            "{}",
            node
        );
    }
}

/// N-dataCommon-10, decision 1: "today" is the IST date on any machine.
/// 00:30 IST on 29 Oct is still 28 Oct in UTC and in New York.
#[tokio::test]
async fn dates_are_ist_whatever_the_machine_clock() {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", json!({"status": "success", "data": []}));
    client.reply("market/timings", json!({"status": "success", "data": []}));
    let c = ctx_at(
        client.clone(),
        "2026-10-29T00:30:00",
        "w",
        NodeEnv::default(),
    );
    run(
        &c,
        "history",
        json!({"symbol": "SBIN", "interval": "D", "days": 2}),
    )
    .await;
    run(&c, "timings", json!({})).await;
    run(&c, "barOffset", json!({"symbol": "SBIN"})).await;
    let calls = client.calls();
    assert_eq!(calls[0].payload["end_date"], "2026-10-29");
    assert_eq!(calls[0].payload["start_date"], "2026-10-27");
    assert_eq!(calls[1].payload["date"], "2026-10-29");
    assert_eq!(calls[2].payload["end_date"], "2026-10-29");
}

/// N-getOrderStatus-02: the desktop's endpoint needs a strategy label; the
/// node sends the workflow's name, as its order nodes tag.
#[tokio::test]
async fn order_status_carries_the_workflow_name() {
    let client = Arc::new(FakeFlowClient::new());
    client.reply("orderstatus", json!({"status": "success", "data": {}}));
    let c = ctx_at(
        client.clone(),
        "2026-10-29T10:15:00",
        "Gap fade",
        NodeEnv::default(),
    );
    run(&c, "getOrderStatus", json!({"orderId": "42"})).await;
    assert_eq!(
        Value::Object(client.calls()[0].payload.clone()),
        json!({"orderid": "42", "strategy": "Gap fade"})
    );
}

/// The registry carries every data node type once (N-<type>-00).
#[test]
fn every_data_node_is_registered() {
    let registry = NodeRegistry::default();
    for t in [
        "getQuote",
        "getDepth",
        "getOrderStatus",
        "openPosition",
        "history",
        "strategyPnl",
        "priorPeriodOhlc",
        "barOffset",
        "indicator",
        "symbol",
        "optionSymbol",
        "expiry",
        "intervals",
        "multiQuotes",
        "optionChain",
        "syntheticFuture",
        "calendar",
        "holidays",
        "timings",
        "orderBook",
        "tradeBook",
        "positionBook",
        "holdings",
        "funds",
        "margin",
    ] {
        assert!(registry.get(t).is_some(), "{}", t);
    }
    assert_eq!((data::FAMILY.build)().len(), 25);
}

/// F-261, IND-076: a node as the editor creates it (interval D, 100 bars,
/// tail 5, offset 0, params `{"period": 14}`, output `ind`) runs and gives
/// the variables workflows read.
#[tokio::test]
async fn the_editors_indicator_defaults_run_and_feed_variables() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", candles(120));
    let c = ctx(client);
    let editor = json!({"indicatorName": "rsi", "symbol": "SBIN", "exchange": "NSE", "interval": "D",
                        "lookbackBars": 100, "tailBars": 5, "offsetBars": 0,
                        "params": "{\"period\": 14}", "outputVariable": "ind"});
    let out = run(&c, "indicator", editor).await;
    assert!(!out.is_error(), "{:?}", out);
    for path in [
        "{{ind.latest.value}}",
        "{{ind.previous.value}}",
        "{{ind.at_offset.value}}",
        "{{ind.series[4].value}}",
        "{{ind.bars_used}}",
    ] {
        let text = c.vars.interpolate(path);
        assert!(!text.contains("{{"), "{} did not resolve", path);
    }
    assert_eq!(
        c.vars.interpolate("{{ind.series[4].value}}"),
        c.vars.interpolate("{{ind.latest.value}}")
    );
    assert_eq!(c.vars.interpolate("{{ind.bars_used}}"), "120");
    assert!(
        matches!(c.vars.resolve_raw(&json!("{{ind.series}}")), Value::Array(ref a) if a.len() == 5)
    );
}

/// Deeply nested JSON text reaching a node (a webhook value interpolated
/// into the Margin basket or an Indicator's sourceSeries) is this node's
/// error, never a crash of the app: decoding stops at Python's recursion
/// depth with its sentence.
#[tokio::test]
async fn deeply_nested_text_is_a_node_error_not_a_crash() {
    let deep = "[".repeat(100_000);
    let client = Arc::new(FakeFlowClient::new());
    let c = ctx(client.clone());
    c.vars.set("webhook", json!({"basket": deep.clone()}));
    let out = run(&c, "margin", json!({"positionsJson": "{{webhook.basket}}"})).await;
    assert!(
        out.failure_message().starts_with(
            "Margin positions is not valid JSON after variable interpolation: maximum recursion depth exceeded"
        ),
        "{}",
        out.failure_message()
    );
    let out = run(
        &c,
        "indicator",
        json!({"indicatorName": "sma", "sourceSeries": deep}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "sourceSeries did not resolve to a JSON array"
    );
    assert!(client.calls().is_empty());
}

/// JSON text that holds a string is "not an array", as Python reads it
/// (json.loads gives a str), not text to parse again.
#[tokio::test]
async fn a_series_of_json_text_holding_a_string_is_not_an_array() {
    let c = ctx(Arc::new(FakeFlowClient::new()));
    let out = run(
        &c,
        "indicator",
        json!({"indicatorName": "sma", "sourceSeries": "\"[1, 2, 3]\""}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "sourceSeries must resolve to an array"
    );
    assert_eq!(
        logs(&c)[0].1,
        "sourceSeries must resolve to an array, got: '[1, 2, 3]'"
    );
}

/// IND-070: a node with no indicator, exchange or interval reads the
/// executor's defaults: sma on NSE daily bars over 100 bars.
#[tokio::test]
async fn an_indicator_without_settings_uses_the_executors_defaults() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", candles(120));
    let c = ctx(client.clone());
    run(&c, "indicator", json!({"symbol": "SBIN"})).await;
    assert_eq!(logs(&c)[0].1, "Computing sma for SBIN (D, 100 bars)");
    assert_eq!(
        Value::Object(client.calls()[0].payload.clone()),
        json!({"symbol": "SBIN", "exchange": "NSE", "interval": "D",
               "start_date": "2026-05-17", "end_date": "2026-10-29", "source": "api"})
    );
}

/// IND-076: a multi-output indicator's variables: `{{name.outputs}}` and
/// `{{name.at_offset.out0}}`.
#[tokio::test]
async fn a_multi_output_indicator_feeds_its_named_outputs() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply("history", candles(120));
    let c = ctx(client);
    let out = run(
        &c,
        "indicator",
        json!({"symbol": "SBIN", "indicatorName": "macd", "params": "", "offsetBars": 1, "outputVariable": "m"}),
    )
    .await;
    assert!(!out.is_error(), "{:?}", out);
    assert_eq!(
        c.vars.interpolate("{{m.outputs}}"),
        "['out0', 'out1', 'out2']"
    );
    for path in [
        "{{m.at_offset.out0}}",
        "{{m.at_offset.out2}}",
        "{{m.latest.out1}}",
    ] {
        let text = c.vars.interpolate(path);
        assert!(text.parse::<f64>().is_ok(), "{} read {}", path, text);
    }
    assert_eq!(
        c.vars.interpolate("{{m.at_offset.out0}}"),
        c.vars.interpolate("{{m.previous.out0}}"),
        "offset 1 is the previous bar"
    );
}

/// N-barOffset-02, decision 21: a history row whose time cannot be read
/// fails only this node (`Bar offset error: ...`); the web raised and ended
/// the whole run.
#[tokio::test]
async fn an_unreadable_bar_time_fails_only_the_node() {
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    client.reply(
        "history",
        json!({"status": "success", "data": [{"timestamp": "garbage", "close": 1.0}]}),
    );
    let c = ctx(client);
    let out = run(
        &c,
        "barOffset",
        json!({"symbol": "SBIN", "outputVariable": "b"}),
    )
    .await;
    assert_eq!(out.aborted(), None, "the run goes on");
    assert!(out.is_error());
    assert_eq!(out.failure_message(), "Invalid isoformat string: 'garbage'");
    assert!(
        logs(&c)
            .iter()
            .any(|(l, m)| l == "error"
                && m == "Bar offset error: Invalid isoformat string: 'garbage'")
    );
    assert_eq!(c.vars.get("b"), None);
}
