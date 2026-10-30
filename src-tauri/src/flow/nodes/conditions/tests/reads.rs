//! Defaults, interpolated fields, read failures and operator coverage of
//! the condition nodes (the audit's full sweep, decision 35).

use super::*;

/// N-positionCheck-01: product and condition are interpolated.
#[tokio::test]
async fn position_check_interpolates_product_and_condition() {
    let (client, c) = ctx();
    client.reply(
        "positionbook",
        json!({"status": "success", "data": [
            {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 10, "pnl": 1},
            {"symbol": "SBIN", "exchange": "NSE", "product": "NRML", "quantity": 3, "pnl": 2}
        ]}),
    );
    c.vars.set("prod", json!("NRML"));
    c.vars.set("cond", json!("quantity_below"));
    let d = json!({"symbol": "SBIN", "exchange": "NSE", "product": "{{prod}}",
                   "condition": "{{cond}}", "threshold": 5});
    let out = run(&PositionCheck, &c, d).await;
    assert_eq!(out.condition_met(), Some(true));
    assert_eq!(answer(&out)["quantity"], 3);
    assert_eq!(messages(&c)[1], "Position check: qty=3 < 5.0 = True");
}

/// A client answer with `error`, `message`, both, or neither.
fn failures() -> [(Value, Option<&'static str>); 4] {
    [
        (
            json!({"status": "error", "error": "E", "message": "M"}),
            Some("E"),
        ),
        (
            json!({"status": "error", "message": "Read timed out"}),
            Some("Read timed out"),
        ),
        (json!({"status": "error", "error": "", "message": ""}), None),
        (json!({"status": "error"}), None),
    ]
}

/// N-positionCheck-04, N-fundCheck-04, N-priceCondition-03: a failed read
/// says the client's `error`, else its `message`, else the node's own
/// sentence, logged after the node's prefix.
#[tokio::test]
async fn a_failed_read_says_error_then_message_then_the_fallback() {
    let nodes: [(&str, &str, &str, &dyn FlowNode, Value); 3] = [
        (
            "positionbook",
            "Position check aborted: ",
            "the position lookup failed",
            &PositionCheck,
            json!({"symbol": "SBIN"}),
        ),
        (
            "funds",
            "Fund check aborted: ",
            "the funds lookup failed",
            &FundCheck,
            json!({"minAvailable": 1}),
        ),
        (
            "quotes",
            "Price check aborted: ",
            "the price lookup failed",
            &PriceCondition,
            json!({"symbol": "SBIN", "value": 1}),
        ),
    ];
    for (endpoint, prefix, fallback, node, d) in nodes {
        for (reply, said) in failures() {
            let (client, c) = ctx();
            client.reply(endpoint, reply.clone());
            let out = run(node, &c, d.clone()).await;
            let message = said.unwrap_or(fallback);
            assert_eq!(
                answer(&out),
                json!({"status": "error", "condition": false, "message": message}),
                "{} {}",
                endpoint,
                reply
            );
            assert_eq!(
                logs(&c).last().cloned(),
                Some(("error".to_string(), format!("{}{}", prefix, message)))
            );
        }
    }
}

/// N-positionCheck-08: a threshold that does not resolve fails closed for
/// every threshold condition, not only quantity_above.
#[tokio::test]
async fn every_threshold_condition_fails_closed_on_an_unreadable_threshold() {
    for condition in ["quantity_above", "quantity_below", "pnl_above", "pnl_below"] {
        let (client, c) = ctx();
        client.reply("positionbook", book());
        let d = json!({"symbol": "SBIN", "product": "MIS", "condition": condition,
                       "threshold": "{{typo}}"});
        let out = run(&PositionCheck, &c, d).await;
        assert_eq!(
            answer(&out),
            json!({"status": "error", "condition": false,
                   "message": "threshold did not resolve to a number: '{{typo}}'"}),
            "{}",
            condition
        );
    }
}

fn cash(available: f64) -> Value {
    json!({"status": "success", "data": {"availablecash": available}})
}

/// N-fundCheck-01: with minAvailable present, a legacy threshold/operator
/// pair is not read (no warning, the minimum decides).
#[tokio::test]
async fn min_available_wins_over_a_legacy_pair() {
    let (client, c) = ctx();
    client.reply("funds", cash(1000.0));
    let d = json!({"minAvailable": 500, "threshold": 1e9, "operator": "lt"});
    let out = run(&FundCheck, &c, d).await;
    assert_eq!(out.condition_met(), Some(true));
    assert_eq!(
        logs(&c),
        vec![
            ("info".to_string(), "Checking funds".to_string()),
            (
                "info".to_string(),
                "Fund check: available=1000.0 >= 500.0 = True".to_string()
            )
        ]
    );
}

/// N-fundCheck-02: every operator a minimum can express is accepted
/// (stripped and lower-cased; none means `gt`), with the legacy warning.
#[tokio::test]
async fn every_minimum_operator_is_accepted() {
    for op in [
        json!("gt"),
        json!("gte"),
        json!("ge"),
        json!(">"),
        json!(">="),
        json!(" GT "),
        Value::Null,
    ] {
        let (client, c) = ctx();
        client.reply("funds", cash(1000.0));
        let d = json!({"threshold": 500, "operator": op});
        let out = run(&FundCheck, &c, d.clone()).await;
        assert_eq!(out.condition_met(), Some(true), "{}", d);
        assert_eq!(
            logs(&c)[0],
            (
                "warning".to_string(),
                "Fund Check is using its legacy threshold of 500.0. Re-save the node to store it as Minimum Available.".to_string()
            ),
            "{}",
            d
        );
    }
}

/// N-priceCondition-01: the field defaults to `ltp` and the exchange to
/// `NSE`.
#[tokio::test]
async fn price_condition_defaults_to_ltp_on_nse() {
    let (client, c) = ctx();
    client.reply("quotes", quote(json!({"ltp": 100.0, "high": 120.0})));
    let out = run(&PriceCondition, &c, json!({"symbol": "SBIN", "value": 50})).await;
    assert_eq!(client.calls_to("quotes")[0].payload["exchange"], "NSE");
    assert_eq!(answer(&out)["field"], "ltp");
    assert_eq!(messages(&c)[1], "Price check: ltp=100.0 > 50.0 = True");
}

/// N-priceCondition-04: the answer's `ltp` reads 0.0 when the quote has
/// none, whatever field was compared.
#[tokio::test]
async fn the_answer_reads_a_missing_ltp_as_zero() {
    let d = json!({"symbol": "S", "field": "high", "operator": ">", "value": 100});
    let (out, _, _) = price(d, quote(json!({"high": 120.0}))).await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "condition": true, "ltp": 0.0,
               "field": "high", "field_value": 120.0})
    );
}

async fn var_answer(d: Value) -> NodeOutcome {
    let (_, c) = ctx();
    run(&VarCondition, &c, d).await
}

/// N-varCondition-02, -03: a boolean operand is read as Python's text,
/// leftValue is checked before rightValue, and both before the operator.
#[tokio::test]
async fn operands_are_checked_left_then_right_then_the_operator() {
    for (d, message) in [
        (
            json!({"leftValue": true, "rightValue": "1"}),
            "leftValue did not resolve to a number: 'True'",
        ),
        (
            json!({"leftValue": "1", "rightValue": false}),
            "rightValue did not resolve to a number: 'False'",
        ),
        (
            json!({"leftValue": "", "rightValue": "x"}),
            "leftValue is empty",
        ),
        (
            json!({"leftValue": "x", "rightValue": "", "operator": "~"}),
            "leftValue did not resolve to a number: 'x'",
        ),
        (
            json!({"leftValue": "1", "rightValue": "y", "operator": "~"}),
            "rightValue did not resolve to a number: 'y'",
        ),
        (
            json!({"leftValue": "1", "rightValue": "2", "operator": "~"}),
            "operator '~' is not one of !=, <, <=, ==, >, >=, eq, gt, gte, lt, lte, neq",
        ),
    ] {
        let out = var_answer(d.clone()).await;
        assert_eq!(out.failure_message(), message, "{}", d);
        assert_eq!(out.condition_met(), Some(false), "{}", d);
    }
}

/// N-varCondition-04: equality is exact on floats (no tolerance), and
/// every operator spelling compares as the web's `_compare`.
#[tokio::test]
async fn comparisons_are_exact_for_every_operator() {
    let sum = "0.30000000000000004";
    for (op, left, right, met) in [
        ("==", sum, "0.3", false),
        ("eq", sum, "0.3", false),
        ("!=", sum, "0.3", true),
        ("neq", sum, "0.3", true),
        (">", sum, "0.3", true),
        ("gt", "1", "1", false),
        ("<", "1", "2", true),
        ("lt", "2", "1", false),
        (">=", "1", "1", true),
        ("gte", "0.9", "1", false),
        ("<=", "1", "1", true),
        ("lte", "1.1", "1", false),
    ] {
        let d = json!({"leftValue": left, "operator": op, "rightValue": right});
        let out = var_answer(d.clone()).await;
        assert_eq!(out.condition_met(), Some(met), "{}", d);
        assert!(!out.is_error(), "{}", d);
    }
}
