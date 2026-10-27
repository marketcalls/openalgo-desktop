//! The dispatcher against the web's own `compute()` (`compute.json`): every
//! entry with its defaults, every borrowed name, coercion and refusal the
//! recorder exercised, the window sizing, the rounding and the scan parser.
//!
//! Tolerance: values are rounded to six places on both sides, so a number
//! matches within `1e-6 + 1e-9 * |expected|` (one unit in the sixth place,
//! for a raw value that lands on a rounding edge; the relative term for
//! large running totals such as OBV). Refusal sentences match exactly.

use serde_json::{Map, Value};

use super::support::{frame, json, json_diff, second, COMPUTE};
use crate::agent::indicators::coerce::{required_bars, resolve_param_aliases};
use crate::agent::indicators::compute::compute;
use crate::agent::indicators::compute::Cell;
use crate::agent::indicators::registry::get_spec;
use crate::agent::indicators::scan::{
    evaluate_condition, parse_condition, Condition, CrossKind, Op, ResultView, CONDITION_HELP,
};
use crate::agent::tools::numbers::py_round;

const ABS: f64 = 1e-6;
const REL: f64 = 1e-9;

fn params(case: &Value) -> Option<Map<String, Value>> {
    case["params"].as_object().cloned()
}

#[test]
fn compute_matches_the_web_case_by_case() {
    let fixture = json(COMPUTE);
    let mut failures = Vec::new();
    let cases = fixture["cases"].as_array().expect("cases");
    for case in cases {
        let label = format!("{} {}", case["label"], case["name"]);
        let data = frame(&fixture["frames"][case["frame"].as_str().expect("frame")]);
        let other = case["second"]
            .as_str()
            .map(|key| second(&fixture["seconds"][key]));
        let given = params(case);
        let outcome = compute(
            case["name"].as_str().expect("name"),
            &data,
            given.as_ref(),
            other.as_ref(),
            case["last_n"].as_i64().expect("last_n"),
        );
        match (outcome, case.get("error")) {
            (Err(e), Some(want)) if want == e.0.as_str() => {}
            (Err(e), Some(want)) => {
                failures.push(format!("{label}:\n  got  {}\n  want {want}", e.0))
            }
            (Err(e), None) => failures.push(format!("{label}: refused: {}", e.0)),
            (Ok(_), Some(want)) => failures.push(format!("{label}: computed, want refusal {want}")),
            (Ok(result), None) => {
                let got = serde_json::to_value(result.to_json()).expect("serialises");
                if let Some(diff) = json_diff("result", &got, &case["result"], ABS, REL) {
                    failures.push(format!("{label}: {diff}"));
                }
            }
        }
    }
    assert!(cases.len() > 200, "only {} compute cases", cases.len());
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn result_keys_come_in_the_web_order() {
    let fixture = json(COMPUTE);
    let data = frame(&fixture["frames"]["trend"]);
    let result = compute("uo_oscillator", &data, None, None, 3).expect("computes");
    let text = serde_json::to_string(&result.to_json()).expect("serialises");
    let order = [
        "\"indicator\"",
        "\"category\"",
        "\"description\"",
        "\"params_used\"",
        "\"bars_used\"",
        "\"defaults_applied\"",
        "\"outputs\"",
        "\"values_returned\"",
        "\"timestamps\"",
        "\"values\"",
        "\"summary\"",
        "\"note\"",
        "\"requested_as\"",
    ];
    let positions: Vec<usize> = order.iter().map(|k| text.find(k).expect(k)).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
    let params = text.find("\"period1\"").expect("period1");
    assert!(params < text.find("\"period3\"").expect("period3"));
}

#[test]
fn window_sizing_matches_the_web() {
    let fixture = json(COMPUTE);
    for case in fixture["required_bars"].as_array().expect("required_bars") {
        let spec = get_spec(case["name"].as_str().expect("name")).expect("spec");
        let given = params(case);
        let want = case["want"].as_u64().expect("want") as usize;
        assert_eq!(
            required_bars(spec, given.as_ref(), want) as u64,
            case["bars"].as_u64().expect("bars"),
            "{case}"
        );
    }
}

#[test]
fn borrowed_names_resolve_as_on_the_web() {
    let fixture = json(COMPUTE);
    for case in fixture["aliases"].as_array().expect("aliases") {
        let spec = get_spec(case["name"].as_str().expect("name")).expect("spec");
        let given = params(case);
        let got: Map<String, Value> = resolve_param_aliases(spec, given.as_ref())
            .into_iter()
            .collect();
        assert_eq!(Value::Object(got), case["resolved"], "{case}");
    }
}

#[test]
fn rounding_is_pythons_round() {
    let fixture = json(COMPUTE);
    for case in fixture["rounding"].as_array().expect("rounding") {
        let value = case["value"].as_f64().expect("value");
        let want = case["rounded"].as_f64().expect("rounded");
        let got = py_round(value, 6);
        assert!(
            got == want && got.is_sign_negative() == want.is_sign_negative(),
            "{value}: {got} != {want}"
        );
    }
}

fn condition_json(parsed: &Option<Condition>) -> Value {
    let Some(parsed) = parsed else {
        return Value::Null;
    };
    match parsed {
        Condition::Compare { left, op, right } => serde_json::json!({
            "kind": "compare",
            "left": left,
            "op": match op { Op::Lt => "<", Op::Le => "<=", Op::Gt => ">", Op::Ge => ">=", Op::Eq => "==" },
            "right": right,
        }),
        Condition::Cross { kind, a, b } => serde_json::json!({
            "kind": "cross",
            "fn": match kind { CrossKind::Over => "crossover", CrossKind::Under => "crossunder", CrossKind::Either => "cross" },
            "a": a,
            "b": b,
        }),
    }
}

#[test]
fn the_condition_parser_accepts_exactly_the_webs_shapes() {
    let fixture = json(COMPUTE);
    assert_eq!(fixture["condition_help"], CONDITION_HELP);
    for case in fixture["conditions"].as_array().expect("conditions") {
        let text = case["text"].as_str().expect("text");
        assert_eq!(
            condition_json(&parse_condition(text)),
            case["parsed"],
            "{text:?}"
        );
    }
}

/// An output's name and its returned values.
type Output = (String, Vec<Cell>);

/// A result dict as the web's evaluate_condition reads it.
fn view_parts(result: &Value) -> (String, Option<Vec<String>>, Vec<Output>) {
    let indicator = result["indicator"].as_str().unwrap_or_default().to_string();
    let outputs = result["outputs"].as_array().map(|a| {
        a.iter()
            .filter_map(|o| o.as_str().map(String::from))
            .collect()
    });
    let values = result["values"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, cells)| {
                    let cells = cells
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|c| match c {
                                    Value::Bool(b) => Cell::Flag(*b),
                                    Value::Number(n) => Cell::Num(n.as_f64().unwrap_or(f64::NAN)),
                                    _ => Cell::Null,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (k.clone(), cells)
                })
                .collect()
        })
        .unwrap_or_default();
    (indicator, outputs, values)
}

#[test]
fn conditions_evaluate_as_on_the_web() {
    let fixture = json(COMPUTE);
    for case in fixture["evaluations"].as_array().expect("evaluations") {
        let condition = case["condition"].as_str().expect("condition");
        let parsed = parse_condition(condition).expect("parses");
        let (indicator, outputs, values) = view_parts(&case["result"]);
        let view = ResultView {
            indicator: &indicator,
            outputs: outputs.as_deref(),
            values: &values,
        };
        let (hit, detail) = evaluate_condition(&parsed, &view);
        assert_eq!(Value::Bool(hit), case["hit"], "{condition}");
        let got = serde_json::to_value(detail).expect("serialises");
        assert_eq!(
            json_diff("detail", &got, &case["detail"], 0.0, 0.0),
            None,
            "{condition}"
        );
    }
}
