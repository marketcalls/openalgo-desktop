//! Math Expression: every expression in `logic_vectors.json` (the web's
//! own `_safe_eval_math` under Python 3.12, see
//! `scripts/gen_flow_logic_vectors.py`) evaluates to the same float or
//! fails with the same sentence, plus the web's tests
//! (`test/test_flow_qa_regressions.py` 1489-1516) and the node's log and
//! result.

use super::*;
use crate::flow::nodes::testkit::{answer, ctx, logs, run};
use serde_json::json;

fn vectors() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/flow/logic_vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const SYNTAX: &str = "Invalid expression syntax: ";

#[test]
fn every_vector_evaluates_as_the_web() {
    let v = vectors();
    let mut checked = 0;
    let mut exact_syntax = 0;
    for case in v["math"].as_array().unwrap() {
        let expr = case["expr"].as_str().unwrap();
        let got = safe_eval(expr).map(pyfmt::float);
        match (case.get("ok"), case.get("err").and_then(Value::as_str)) {
            (Some(ok), _) => assert_eq!(got, Ok(ok.as_str().unwrap().to_string()), "{:?}", expr),
            (_, Some(err)) if err.starts_with(SYNTAX) => {
                // Lead decision 5: the prefix is kept, Python's detail is
                // the parser's own (most match anyway).
                let e = got.expect_err(expr);
                assert!(e.starts_with(SYNTAX), "{:?}: {}", expr, e);
                if e == err {
                    exact_syntax += 1;
                }
            }
            (_, Some(err)) => assert_eq!(got, Err(err.to_string()), "{:?}", expr),
            _ => panic!("bad vector {}", case),
        }
        checked += 1;
    }
    assert!(checked > 300, "{}", checked);
    assert!(exact_syntax >= 20, "{}", exact_syntax);
}

/// X-213, X-214: `floor` is the one call, with exactly one positional
/// argument.
#[test]
fn floor_is_the_only_call() {
    assert_eq!(safe_eval("floor(3.9)"), Ok(3.0));
    assert_eq!(safe_eval("floor(2 + 2.8)"), Ok(4.0));
    for refused in [
        "ceil(1.1)",
        "math.floor(2.2)",
        "floor()",
        "floor(1, 2)",
        "floor(x=1)",
        "floor(unknown(1))",
        "__import__('os').system('echo unsafe')",
    ] {
        assert_eq!(
            safe_eval(refused),
            Err("Only floor(expression) is supported".into()),
            "{}",
            refused
        );
    }
}

/// N-mathExpression-07: Python's arithmetic, not f64's.
#[test]
fn python_semantics_hold() {
    assert_eq!(safe_eval("-2**2"), Ok(-4.0));
    assert_eq!(safe_eval("2**3**2"), Ok(512.0));
    assert_eq!(safe_eval("7/2"), Ok(3.5));
    assert_eq!(safe_eval("-7%3"), Ok(2.0));
    assert_eq!(safe_eval("7%-3"), Ok(-2.0));
    assert_eq!(safe_eval("-7.5%2"), Ok(0.5));
    assert_eq!(safe_eval("(2**60+1)-2**60"), Ok(1.0));
    assert_eq!(safe_eval("floor(-2.5)"), Ok(-3.0));
    assert_eq!(
        safe_eval("(-8) ** 0.5"),
        Err("float() argument must be a string or a real number, not 'complex'".into())
    );
    assert_eq!(safe_eval("1e308*10"), Ok(f64::INFINITY));
}

/// Nesting is refused before it can overflow a tokio worker's 2 MiB
/// stack (debug build frames).
#[test]
fn deep_nesting_is_refused_not_overflowed() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(deep_nesting_cases)
        .unwrap()
        .join()
        .unwrap();
}

fn deep_nesting_cases() {
    let deep = format!("{}1{}", "(".repeat(60), ")".repeat(60));
    assert!(safe_eval(&deep).unwrap_err().starts_with(SYNTAX));
    let long = vec!["1"; 5000].join("+");
    assert_eq!(
        safe_eval(&long),
        Err("maximum recursion depth exceeded".into())
    );
    let unary = format!("{}1", "-".repeat(5000));
    assert_eq!(
        safe_eval(&unary),
        Err("maximum recursion depth exceeded".into())
    );
    assert_eq!(safe_eval(&vec!["1"; 150].join("+")), Ok(150.0));
    let nested = format!("{}-1{}", "(-".repeat(49), ")".repeat(49));
    assert_eq!(safe_eval(&nested), Ok(1.0));
    let powers = format!("{}2{}", "(1 ** ".repeat(49), ")".repeat(49));
    assert_eq!(safe_eval(&powers), Ok(1.0));
}

#[tokio::test]
async fn the_node_logs_stores_and_answers_as_the_web() {
    let (_, c) = ctx();
    c.vars.set("ltp", json!(812.5));
    c.vars.set("lot", json!(10));
    c.vars.set("brokerage", json!("20"));
    let out = run(
        &MathExpression,
        &c,
        json!({"expression": "({{ltp}} * {{lot}}) + {{brokerage}}", "outputVariable": "cost"}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "expression": "({{ltp}} * {{lot}}) + {{brokerage}}",
               "interpolated": "(812.5 * 10) + 20", "result": 8145.0, "outputVariable": "cost"})
    );
    assert_eq!(c.vars.get("cost"), Some(json!(8145.0)));
    assert_eq!(
        logs(&c),
        vec![
            (
                "info".into(),
                "Evaluating: ({{ltp}} * {{lot}}) + {{brokerage}}".into()
            ),
            ("info".into(), "Interpolated: (812.5 * 10) + 20".into()),
            ("info".into(), "Result: cost = 8145.0".into()),
        ]
    );
    let out = run(&MathExpression, &c, json!({"expression": "2 + 3"})).await;
    assert_eq!(answer(&out)["outputVariable"], "result");
    assert_eq!(c.vars.get("result"), Some(json!(5.0)));
}

#[tokio::test]
async fn failures_store_nothing() {
    let (_, c) = ctx();
    let out = run(&MathExpression, &c, json!({"expression": ""})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "No expression provided"})
    );
    assert_eq!(
        logs(&c),
        vec![("error".into(), "No expression provided".into())]
    );
    let (_, c) = ctx();
    let out = run(
        &MathExpression,
        &c,
        json!({"expression": "1 / 0", "outputVariable": "x"}),
    )
    .await;
    assert_eq!(out.failure_message(), "division by zero");
    assert_eq!(logs(&c)[2].1, "Math expression failed: division by zero");
    assert!(!c.vars.contains("x"));
    // N-mathExpression-12: a number from imported JSON.
    let out = run(&MathExpression, &c, json!({"expression": 42})).await;
    assert_eq!(
        out.failure_message(),
        "'int' object has no attribute 'strip'"
    );
}

/// N-mathExpression-11: a missing variable is named. The web parsed
/// `{{x}}` as a set and said `Unsupported expression type: Set` (fails on
/// the web).
#[tokio::test]
async fn an_unresolved_variable_is_named() {
    let (_, c) = ctx();
    let out = run(
        &MathExpression,
        &c,
        json!({"expression": "{{missing}} * 2"}),
    )
    .await;
    assert_eq!(out.failure_message(), "unresolved variable {{missing}}");
    assert_eq!(
        logs(&c)[2].1,
        "Math expression failed: unresolved variable {{missing}}"
    );
    let out = run(
        &MathExpression,
        &c,
        json!({"expression": "({{quote.data.ltp}} * {{lotSize}}) + {{brokerage}}"}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "unresolved variables {{quote.data.ltp}}, {{lotSize}}, {{brokerage}}"
    );
    // A dict variable is still the web's refusal.
    c.vars.set("cfg", json!({"a": 1}));
    let out = run(&MathExpression, &c, json!({"expression": "{{cfg}}"})).await;
    assert_eq!(out.failure_message(), "Unsupported expression type: Dict");
}

#[tokio::test]
async fn a_non_finite_result_is_kept_as_python_writes_it() {
    let (_, c) = ctx();
    let out = run(
        &MathExpression,
        &c,
        json!({"expression": "1e308 * 10", "outputVariable": "x"}),
    )
    .await;
    assert_eq!(answer(&out)["result"], "inf");
    assert_eq!(c.vars.interpolate("{{x}}"), "inf");
    assert_eq!(logs(&c)[2].1, "Result: x = inf");
}
