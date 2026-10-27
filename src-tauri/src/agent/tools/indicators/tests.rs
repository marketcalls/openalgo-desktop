//! The indicator tools through a run's path (the schema check, then the
//! tool) against a signed-in OpenAlgo whose mock broker serves the golden
//! trend candles, so a value the tool returns can be checked against the
//! web's own `compute()` over the same bars. Ports the web's
//! `TestIndicatorToolkitArguments` (`test/test_agent_viz_tool_arguments.py`).

use super::args::{batch_requests, params, scan_symbols, tail};
use super::GROUP;
use crate::agent::contract::tool::{Surface, ToolOutcome};
use crate::agent::indicators::tests::support::{json as fixture, shape, COMPUTE};
use crate::agent::tools::market::normalise::{lookback_range, today};
use crate::agent::tools::spec_contract;
use crate::agent::tools::testkit::{body, call_checked, tag, Kit};
use crate::brokers::mock::MockCall;
use crate::brokers::types::Candle;
use serde_json::{json, Map, Value};

/// The golden trend shape as broker candles.
fn trend_candles() -> Vec<Candle> {
    let s = shape("trend");
    (0..s.timestamps.len())
        .map(|i| Candle {
            timestamp: s.timestamps[i],
            open: s.columns["open"][i],
            high: s.columns["high"][i],
            low: s.columns["low"][i],
            close: s.columns["close"][i],
            volume: s.columns["volume"][i] as i64,
            oi: 0,
        })
        .collect()
}

fn kit_with_trend() -> Kit {
    let kit = Kit::new();
    *kit.mock.history.lock() = Some(Ok(trend_candles()));
    kit
}

fn history_calls(kit: &Kit) -> usize {
    kit.mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::History(_)))
        .count()
}

/// The web's `compute()` result for `name` with its defaults on the trend.
fn golden(name: &str) -> Value {
    fixture(COMPUTE)["cases"]
        .as_array()
        .and_then(|cases| {
            cases
                .iter()
                .find(|c| c["label"] == "default" && c["name"] == name)
                .map(|c| c["result"].clone())
        })
        .unwrap_or_else(|| panic!("no golden for {name}"))
}

fn close_enough(got: &Value, want: &Value) -> bool {
    match (got.as_f64(), want.as_f64()) {
        (Some(g), Some(w)) => (g - w).abs() <= 1e-6 + 1e-9 * w.abs(),
        _ => got == want,
    }
}

fn map(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

#[tokio::test]
async fn the_toolkit_is_the_webs() {
    let kit = Kit::new();
    spec_contract::assert_toolkit(&GROUP, &kit.ctx(Surface::Chat));
}

#[tokio::test]
async fn compute_indicator_gives_the_webs_numbers_for_the_same_bars() {
    let kit = kit_with_trend();
    let args =
        json!({"symbol": "INFY", "exchange": "NSE", "indicator": "RSI", "lookback_bars": 300});
    let out = kit.call(&GROUP, "compute_indicator", args).await;
    assert!(out.is_ok(), "{:?}", out);
    assert!(tag(&out).contains("indicator=\"rsi\""), "{}", tag(&out));
    let b = body(&out);
    let want = golden("rsi");
    assert_eq!(b["bars_computed_over"], 300);
    assert_eq!(b["bars_fetched"], 300);
    assert_eq!(b["interval"], "D");
    assert_eq!(b["timezone"], "Asia/Kolkata");
    assert_eq!(b["timestamps"], want["timestamps"]);
    assert_eq!(b["candles"].as_array().map(Vec::len), Some(10));
    let got = b["values"]["rsi"].as_array().cloned().unwrap_or_default();
    let expected = want["values"]["rsi"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(got.len(), expected.len());
    assert!(
        got.iter().zip(&expected).all(|(g, w)| close_enough(g, w)),
        "{:?} vs {:?}",
        got,
        expected
    );
    assert_eq!(
        b["summary"]["rsi"]["finite_count"],
        want["summary"]["rsi"]["finite_count"]
    );
    assert_eq!(b["params_used"], json!({"period": 14}));
}

/// The web returns a result in its dict order; a MACD table must list its
/// lines as the library returns them, whatever serde_json does with maps.
#[tokio::test]
async fn a_multi_output_result_keeps_the_webs_order() {
    let kit = kit_with_trend();
    let args = json!({"symbol": "INFY", "exchange": "NSE", "indicator": "macd", "last_n": 3});
    let out = kit.call(&GROUP, "compute_indicator", args).await;
    let text = out.text();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {text}"))
    };
    let values = at("\"values\":{");
    assert!(values < at("\"macd_line\":[") && at("\"macd_line\":[") < at("\"signal_line\":["));
    assert!(at("\"signal_line\":[") < at("\"histogram\":["), "{text}");
    let keys = [
        "\"symbol\"",
        "\"exchange\"",
        "\"interval\"",
        "\"source\"",
        "\"timezone\"",
        "\"bars_computed_over\"",
        "\"bars_fetched\"",
        "\"values_returned\"",
        "\"timestamps\"",
        "\"candles\"",
        "\"note\"",
        "\"indicator\"",
        "\"category\"",
        "\"description\"",
        "\"params_used\"",
        "\"bars_used\"",
        "\"defaults_applied\"",
        "\"outputs\"",
        "\"values\"",
        "\"summary\"",
    ];
    let positions: Vec<usize> = keys.iter().map(|k| at(k)).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
    assert!(
        at("\"fast_period\"") < at("\"slow_period\"")
            && at("\"slow_period\"") < at("\"signal_period\"")
    );
}

#[tokio::test]
async fn a_second_indicator_on_the_same_instrument_reuses_the_candles() {
    let kit = kit_with_trend();
    let ctx = kit.ctx(Surface::Chat);
    let tools = (GROUP.build)(&ctx);
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t.spec().name == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}"))
    };
    let first =
        json!({"symbol": "INFY", "exchange": "NSE", "indicator": "sma", "params": {"period": 50}});
    let out = call_checked(tool("compute_indicator").as_ref(), &ctx, &first).await;
    assert_eq!(body(&out)["bars_fetched"], 300, "{:?}", out);
    let second = json!({"symbol": "INFY", "exchange": "NSE", "indicators": "rsi, macd"});
    let out = call_checked(tool("compute_indicators_batch").as_ref(), &ctx, &second).await;
    let b = body(&out);
    assert_eq!(b["bars_fetched"], 0, "{b}");
    assert_eq!(b["results"].as_array().map(Vec::len), Some(2));
    assert_eq!(history_calls(&kit), 1);
}

#[tokio::test]
async fn a_batch_reports_each_failure_by_name() {
    let kit = kit_with_trend();
    let args = json!({"symbol": "INFY", "exchange": "NSE", "indicators": ["rsi", {"indicator": "macd"}, "beta"]});
    let out = kit.call(&GROUP, "compute_indicators_batch", args).await;
    let b = body(&out);
    assert!(tag(&out).contains("count=\"2\""), "{}", tag(&out));
    assert_eq!(b["results"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        b["failed"],
        json!([{"indicator": "beta", "error": "beta compares two instruments and needs a second one. Supply compare_symbol and compare_exchange."}])
    );
    assert!(b["results"][0].get("timestamps").is_none());

    let only = json!({"symbol": "INFY", "exchange": "NSE", "indicators": "crossover"});
    let refused = kit.call(&GROUP, "compute_indicators_batch", only).await;
    assert!(!refused.is_ok());
    assert!(
        refused
            .text()
            .starts_with("crossover cannot be computed directly."),
        "{}",
        refused.text()
    );
}

#[tokio::test]
async fn beta_needs_a_benchmark_and_fetches_it() {
    let kit = kit_with_trend();
    let bare = json!({"symbol": "INFY", "exchange": "NSE", "indicator": "beta"});
    let refused = kit.call(&GROUP, "compute_indicator", bare).await;
    assert_eq!(
        refused,
        ToolOutcome::Retry(
            "The 'compare_symbol' argument is invalid: it is empty, and beta compares two instruments Pass the benchmark as well, for example compare_symbol='NIFTY', compare_exchange='NSE_INDEX'.".into()
        )
    );
    let args = json!({"symbol": "INFY", "exchange": "NSE", "indicator": "beta", "compare_symbol": "NIFTY", "compare_exchange": "NSE_INDEX", "last_n": 2});
    let out = kit.call(&GROUP, "compute_indicator", args).await;
    let b = body(&out);
    assert_eq!(b["bars_fetched"], 600, "{b}");
    assert_eq!(b["bars_computed_over"], 284);
    assert!(b["values"]["beta"][1].as_f64().is_some(), "{b}");
}

#[tokio::test]
async fn the_web_scan_matches_both_instruments_and_refuses_prose() {
    let kit = kit_with_trend();
    let args = json!({"symbols": "SBIN, INFY", "exchange": "NSE", "indicator": "rsi", "condition": "RSI < 101", "params": "{\"length\": 14}"});
    let out = kit.call(&GROUP, "scan_symbols", args).await;
    let b = body(&out);
    assert_eq!(b["matched_count"], 2, "{b}");
    assert_eq!(b["scanned"], 2);
    assert_eq!(b["matches"][0]["symbol"], "SBIN");
    assert_eq!(b["matches"][0]["condition_met"], true);
    assert_eq!(b["params_used"], json!({"length": 14}));
    assert!(tag(&out).contains("matched=\"2\""), "{}", tag(&out));
    let prose = json!({"symbols": ["SBIN"], "exchange": "NSE", "indicator": "rsi", "condition": "RSI between 20 and 30"});
    let refused = kit.call(&GROUP, "scan_symbols", prose).await;
    assert!(
        refused
            .text()
            .starts_with("The 'condition' argument is invalid: 'RSI between 20 and 30'"),
        "{}",
        refused.text()
    );
}

#[tokio::test]
async fn a_scan_keeps_going_past_an_instrument_it_cannot_read() {
    let kit = kit_with_trend();
    let args = json!({"symbols": ["INFY", "NSE:"], "exchange": "NSE", "indicator": "rsi", "condition": "rsi > 0"});
    let out = kit.call(&GROUP, "scan_symbols", args).await;
    let b = body(&out);
    assert_eq!(b["scanned"], 1, "{b}");
    let failed = b["failed"][0]["error"].as_str().unwrap_or_default();
    assert!(!failed.is_empty() && failed.chars().count() <= 200, "{b}");
}

#[tokio::test]
async fn no_candles_is_refused_with_the_range_asked_for() {
    let kit = Kit::new();
    let args = json!({"symbol": "INFY", "exchange": "NSE", "indicator": "rsi"});
    let out = kit.call(&GROUP, "compute_indicator", args).await;
    let (start, end) = lookback_range("D", 60, today(kit.app()));
    assert_eq!(
        out,
        ToolOutcome::Retry(format!(
            "No usable candles came back for INFY on NSE at D between {} and {}. Check the symbol and exchange, confirm the interval is one this broker serves, and remember an index is quoted on an index exchange.",
            start, end
        ))
    );
}

#[tokio::test]
async fn discovery_answers_and_refuses_as_the_web() {
    let kit = Kit::new();
    let all = body(&kit.call(&GROUP, "list_indicators", json!({})).await);
    assert_eq!(all["total"], 127);
    assert_eq!(all["detail_omitted"], true);
    let narrow = body(
        &kit.call(&GROUP, "list_indicators", json!({"query": "money flow"}))
            .await,
    );
    assert_eq!(narrow["total"], 4);
    let described = kit
        .call(&GROUP, "describe_indicator", json!({"name": " MACD "}))
        .await;
    assert!(
        tag(&described).contains("name=\"macd\""),
        "{}",
        tag(&described)
    );
    assert_eq!(
        body(&described)["outputs"],
        json!(["macd_line", "signal_line", "histogram"])
    );
    let unknown = kit
        .call(
            &GROUP,
            "compute_indicator",
            json!({"symbol": "INFY", "exchange": "NSE", "indicator": "bollinger"}),
        )
        .await;
    assert_eq!(
        unknown.text(),
        "The 'indicator' argument is invalid: 'bollinger' is not an indicator on this build Did you mean: bbands, bbpercent, bbwidth, obv_smoothed?"
    );
}

#[tokio::test]
async fn the_caps_are_refused_in_the_webs_words() {
    let kit = kit_with_trend();
    let base = |extra: Value| {
        let mut args = map(json!({"symbol": "INFY", "exchange": "NSE", "indicator": "rsi"}));
        args.extend(map(extra));
        Value::Object(args)
    };
    let too_many = kit
        .call(&GROUP, "compute_indicator", base(json!({"last_n": 121})))
        .await;
    assert_eq!(too_many.text(), "The 'last_n' argument is invalid: 121 values per output is more than the 120 one result carries Ask for at most 120, or narrow the question. Every value costs context, and a table longer than this is not read.");
    let too_long = kit
        .call(
            &GROUP,
            "compute_indicator",
            base(json!({"lookback_bars": 5001})),
        )
        .await;
    assert_eq!(too_long.text(), "The 'lookback_bars' argument is invalid: 5001 candles is more than the 5000 one call computes over Ask for at most 5000, or leave it 0 and let the indicator's own warm-up size the window, which is what you want unless you are deliberately looking further back.");
    let nine: Vec<String> = (5..14)
        .map(|p| format!("{{\"name\": \"sma\", \"params\": {{\"period\": {p}}}}}"))
        .collect();
    let batch =
        json!({"symbol": "INFY", "exchange": "NSE", "indicators": format!("[{}]", nine.join(","))});
    let refused = kit.call(&GROUP, "compute_indicators_batch", batch).await;
    assert_eq!(refused.text(), "The 'indicators' argument is invalid: it carries 9 entries, more than the 8 one call computes Split it into batches of at most 8.");
    let sixteen: Vec<String> = (0..16).map(|i| format!("S{i}")).collect();
    let scan =
        json!({"symbols": sixteen, "exchange": "NSE", "indicator": "rsi", "condition": "rsi < 30"});
    let refused = kit.call(&GROUP, "scan_symbols", scan).await;
    assert_eq!(refused.text(), "The 'symbols' argument is invalid: it carries 16 instruments, more than the 15 one scan covers Scan at most 15 at a time; each one is a separate broker history call behind a rate limiter.");
}

#[tokio::test]
async fn parameters_arrive_as_floats_nulls_and_json_text() {
    let kit = kit_with_trend();
    for p in [
        json!({"multiplier": 2.5, "period": null}),
        json!("{\"period\": 7}"),
    ] {
        let args =
            json!({"symbol": "INFY", "exchange": "NSE", "indicator": "supertrend", "params": p});
        let out = kit.call(&GROUP, "compute_indicator", args).await;
        assert!(out.is_ok(), "{:?}", out);
    }
    let one_object = json!({"symbol": "INFY", "exchange": "NSE", "indicators": {"name": "rsi"}});
    assert!(kit
        .call(&GROUP, "compute_indicators_batch", one_object)
        .await
        .is_ok());
    let one_symbol =
        json!({"symbols": "SBIN", "exchange": "NSE", "indicator": "rsi", "condition": "rsi > 0"});
    assert!(kit.call(&GROUP, "scan_symbols", one_symbol).await.is_ok());
}

#[test]
fn params_sent_as_json_text_are_decoded() {
    assert_eq!(
        params(Some(&json!("{\"period\": 21}")), "params").ok(),
        Some(map(json!({"period": 21})))
    );
    assert_eq!(params(None, "params").ok(), Some(Map::new()));
}

#[test]
fn params_text_that_is_not_an_object_is_refused() {
    let refused = params(Some(&json!("period 21")), "params")
        .err()
        .map(|r| r.text().to_string());
    assert_eq!(
        refused.as_deref(),
        Some("The 'params' argument is invalid: 'period 21' is not a set of named parameters Pass an object, for example {\"period\": 21}.")
    );
}

#[test]
fn batch_entries_arrive_as_text_json_or_one_object() {
    let named = |items: &[(&str, Value)]| -> Vec<(String, Map<String, Value>)> {
        items
            .iter()
            .map(|(n, p)| (n.to_string(), map(p.clone())))
            .collect()
    };
    assert_eq!(
        batch_requests(Some(&json!("rsi, macd"))).ok(),
        Some(named(&[("rsi", json!({})), ("macd", json!({}))]))
    );
    let text = json!("[{\"name\": \"sma\", \"params\": \"{\\\"period\\\": 50}\"}]");
    assert_eq!(
        batch_requests(Some(&text)).ok(),
        Some(named(&[("sma", json!({"period": 50}))]))
    );
    assert_eq!(
        batch_requests(Some(&json!({"name": "rsi"}))).ok(),
        Some(named(&[("rsi", json!({}))]))
    );
    let repeated = json!(["rsi", {"name": "RSI"}, {"name": "rsi", "params": {"period": 9}}]);
    assert_eq!(
        batch_requests(Some(&repeated)).map(|r| r.len()).ok(),
        Some(2)
    );
}

#[test]
fn an_unknown_batch_name_is_still_refused() {
    let refused = batch_requests(Some(&json!("rsi, not_an_indicator"))).err();
    assert!(refused.is_some_and(|r| r
        .text()
        .starts_with("The 'indicators' argument is invalid: 'not_an_indicator'")));
}

#[test]
fn small_arguments_read_as_python_reads_them() {
    assert_eq!(tail(Some(&json!("12")), 10).ok(), Some(12));
    assert_eq!(tail(Some(&json!(7.9)), 10).ok(), Some(7));
    assert_eq!(tail(Some(&json!(0)), 10).ok(), Some(10));
    assert_eq!(tail(Some(&json!("ten")), 10).ok(), Some(10));
    assert_eq!(
        scan_symbols(Some(&json!(["sbin", "SBIN", " infy "]))).ok(),
        Some(vec!["SBIN".to_string(), "INFY".into()])
    );
    assert!(scan_symbols(Some(&json!(123))).is_err());
}
