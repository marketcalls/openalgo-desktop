//! The validators against the web's own answers.
//!
//! `tests/fixtures/agent/order_arguments.json` was recorded by running the
//! web's `OrdersToolkit` validators and `order_vocab`
//! (`scripts/agent/capture_web_orders.py`): for each set of arguments, the
//! payload built or the exact correction the model reads.

use super::*;
use serde_json::Value;

const VECTORS: &str = include_str!("../../../../../tests/fixtures/agent/order_arguments.json");

fn answer<T: serde::Serialize>(r: Check<T>) -> Value {
    match r {
        Ok(v) => json!({"ok": v}),
        Err(e) => json!({"retry": e.text()}),
    }
}

fn opt(v: &Value) -> Option<String> {
    text_of(v)
}

fn run(fn_name: &str, args: &Value) -> Value {
    let a = args.as_object().cloned().unwrap_or_default();
    let v = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
    match fn_name {
        "build_order" => answer(build_order(&a, false)),
        "build_order_zero_ok" => answer(build_order(&a, true)),
        "order_id" => answer(order_id(&v("value"))),
        "position_size" => answer(whole_number("position_size", &v("value"), None)),
        "canonical_product" => json!({"ok": canonical_product(opt(&v("value")).as_deref())}),
        "canonical_price_type" => json!({"ok": canonical_price_type(opt(&v("value")).as_deref())}),
        "canonical_action" => json!({"ok": canonical_action(opt(&v("value")).as_deref())}),
        "split_exchange_prefix" => answer(
            split_exchange_prefix(opt(&v("symbol")).as_deref(), opt(&v("exchange")).as_deref())
                .map(|(s, e)| vec![s, e]),
        ),
        "index_refusal" => json!({"ok": index_refusal(v("exchange").as_str().unwrap_or(""))}),
        "refusal_message" => json!({"ok": refusal_message(v("detail").as_str().unwrap_or(""))}),
        other => panic!("no such vector function {}", other),
    }
}

#[test]
fn every_answer_matches_the_webs() {
    let fixture: Value = serde_json::from_str(VECTORS).unwrap();
    assert_eq!(fixture["strategy"], AGENT_STRATEGY);
    let vectors = fixture["vectors"].as_array().unwrap();
    for (i, vector) in vectors.iter().enumerate() {
        let name = vector["fn"].as_str().unwrap();
        let got = run(name, &vector["args"]);
        assert_eq!(got, vector["expect"], "vector {} {} {}", i, name, vector["args"]);
    }
    assert!(vectors.len() > 90, "{}", vectors.len());
}
