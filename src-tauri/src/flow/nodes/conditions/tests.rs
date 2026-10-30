//! Position Check, Fund Check, Price Condition and Var Condition, ported
//! from the web's `test/test_flow_lifecycle_guards.py` (37-64),
//! `test/test_flow_final_fix.py` (236-243) and
//! `test/test_flow_qa_regressions.py` (542-566, 1293-1357, 1687-1720),
//! with the web defects this lane fixes pinned against the web's answer.

use super::*;
use crate::flow::nodes::testkit::{answer, ctx, logs, messages, run};

const BOOK: &str = r#"{"status": "success", "data": [
    {"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": 10, "pnl": 125.0},
    {"symbol": "NIFTY28AUG2624000CE", "exchange": "NFO", "product": "NRML", "quantity": -65, "pnl": 747.5}
]}"#;

fn book() -> Value {
    serde_json::from_str(BOOK).unwrap()
}

/// X-069: the default product follows the exchange (NRML on NFO).
#[tokio::test]
async fn position_check_reads_the_position_it_guards() {
    let (client, c) = ctx();
    client.reply("positionbook", book());
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "NIFTY28AUG2624000CE", "exchange": "NFO", "condition": "exists"}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "condition": true, "quantity": -65, "pnl": 747.5})
    );
    let (client, c) = ctx();
    client.reply(
        "positionbook",
        json!({"status": "success", "data": [
            {"symbol": "NIFTY28AUG2624000CE", "exchange": "NFO", "product": "MIS", "quantity": 5, "pnl": 1}
        ]}),
    );
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "NIFTY28AUG2624000CE", "exchange": "NFO", "condition": "exists"}),
    )
    .await;
    // The MIS row is not the NRML position the node guards.
    assert_eq!(answer(&out)["quantity"], 0);
}

#[tokio::test]
async fn position_conditions_and_their_lines() {
    let cases = [
        (
            json!({"condition": "exists"}),
            true,
            "Position check: qty=10 != 0 = True",
        ),
        (
            json!({"condition": "not_exists"}),
            false,
            "Position check: qty=10 == 0 = False",
        ),
        (
            json!({"condition": "quantity_above", "threshold": 5}),
            true,
            "Position check: qty=10 > 5.0 = True",
        ),
        (
            json!({"condition": "quantity_below", "threshold": "{{limit}}"}),
            true,
            "Position check: qty=10 < 20.0 = True",
        ),
        (
            json!({"condition": "pnl_above", "threshold": 100}),
            true,
            "Position check: pnl=125.0 > 100.0 = True",
        ),
        (
            json!({"condition": "pnl_below", "threshold": -50.5}),
            false,
            "Position check: pnl=125.0 < -50.5 = False",
        ),
        (
            json!({"condition": "exists", "threshold": "{{typo}}"}),
            true,
            "Position check: qty=10 != 0 = True",
        ),
    ];
    for (mut d, met, line) in cases {
        let (client, c) = ctx();
        client.reply("positionbook", book());
        c.vars.set("limit", json!(20));
        d["symbol"] = json!("SBIN");
        d["exchange"] = json!("NSE");
        d["product"] = json!("MIS");
        let out = run(&PositionCheck, &c, d.clone()).await;
        assert_eq!(out.condition_met(), Some(met), "{}", d);
        assert_eq!(
            messages(&c),
            vec!["Checking position for: SBIN".to_string(), line.to_string()],
            "{}",
            d
        );
    }
}

/// E-48, X-097, X-172, X-204: a failed read, no symbol or an unknown
/// condition answers neither branch.
#[tokio::test]
async fn position_check_refuses_rather_than_reading_zero() {
    let (client, c) = ctx();
    client.reply_error("positionbook", "Broker session expired", 401);
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "SBIN", "condition": "not_exists"}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "condition": false, "message": "Broker session expired"})
    );
    assert_eq!(
        logs(&c)[1].1,
        "Position check aborted: Broker session expired"
    );
    let (_, c) = ctx();
    let out = run(&PositionCheck, &c, json!({"condition": "not_exists"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "condition": false,
               "message": "Position Check has no symbol set, so it cannot guard anything."})
    );
    let (client, c) = ctx();
    client.reply("positionbook", book());
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "SBIN", "condition": "qty_gt"}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "condition 'qty_gt' is not one this node can evaluate (exists, not_exists, quantity_above, quantity_below, pnl_above, pnl_below)"
    );
    assert_eq!(out.condition_met(), Some(false));
    let (client, c) = ctx();
    // X-205: a known condition still evaluates.
    client.reply(
        "positionbook",
        json!({"status": "success", "data": [{"symbol": "S", "exchange": "NSE", "product": "MIS", "quantity": 5, "pnl": 100}]}),
    );
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "S", "condition": "exists"}),
    )
    .await;
    assert_eq!(out.condition_met(), Some(true));
}

/// N-positionCheck-08: a decimal quantity is a number, and an unreadable
/// threshold fails closed. The web aborted the run on `int("10.0")` and
/// compared against 0.0 for `{{typo}}` (both fail on the web).
#[tokio::test]
async fn position_check_reads_decimals_and_refuses_an_unreadable_threshold() {
    let (client, c) = ctx();
    client.reply(
        "positionbook",
        json!({"status": "success", "data": [{"symbol": "SBIN", "exchange": "NSE", "product": "MIS", "quantity": "10.0", "pnl": "5"}]}),
    );
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "SBIN", "product": "MIS"}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "condition": true, "quantity": 10, "pnl": 5.0})
    );
    let (client, c) = ctx();
    client.reply("positionbook", book());
    let out = run(
        &PositionCheck,
        &c,
        json!({"symbol": "SBIN", "product": "MIS", "condition": "quantity_above", "threshold": "{{typo}}"}),
    )
    .await;
    assert_eq!(out.condition_met(), Some(false));
    assert!(out.is_error());
    assert_eq!(
        out.failure_message(),
        "threshold did not resolve to a number: '{{typo}}'"
    );
    assert_eq!(client.calls_to("positionbook").len(), 1);
}

fn funds(cash: Value) -> Value {
    json!({"status": "success", "data": {"availablecash": cash}})
}

/// X-170, X-171: no minimum fails closed; a minimum still compares.
#[tokio::test]
async fn fund_check_compares_against_its_minimum() {
    let (client, c) = ctx();
    client.reply("funds", funds(json!(50)));
    let out = run(&FundCheck, &c, json!({})).await;
    assert_eq!(out.condition_met(), Some(false));
    assert!(out.is_error());
    assert!(client.calls().is_empty());
    for (min, met) in [(10000, false), (10, true)] {
        let (client, c) = ctx();
        client.reply("funds", funds(json!(50)));
        let out = run(&FundCheck, &c, json!({"minAvailable": min})).await;
        assert_eq!(out.condition_met(), Some(met));
        assert_eq!(
            messages(&c)[1],
            format!("Fund check: available=50.0 >= {}.0 = {}", min, py_bool(met))
        );
    }
    let (client, c) = ctx();
    client.reply("funds", json!({"status": "success", "data": {}}));
    let out = run(&FundCheck, &c, json!({"minAvailable": 0})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "condition": true, "available": 0.0})
    );
}

/// E-48, X-098: a failed funds read answers neither branch.
#[tokio::test]
async fn fund_check_refuses_on_a_failed_read() {
    let (client, c) = ctx();
    client.reply_error("funds", "Broker session expired", 401);
    let out = run(
        &FundCheck,
        &c,
        json!({"operator": ">=", "minAvailable": 10000}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "condition": false, "message": "Broker session expired"})
    );
    assert_eq!(
        messages(&c)[1],
        "Fund check aborted: Broker session expired"
    );
}

/// N-fundCheck-02: the legacy threshold and operator.
#[tokio::test]
async fn fund_check_reads_a_legacy_threshold() {
    let (client, c) = ctx();
    client.reply("funds", funds(json!(250000.5)));
    let out = run(
        &FundCheck,
        &c,
        json!({"threshold": 1000, "operator": "GTE "}),
    )
    .await;
    assert_eq!(out.condition_met(), Some(true));
    assert_eq!(
        logs(&c)[0],
        (
            "warning".into(),
            "Fund Check is using its legacy threshold of 1000.0. Re-save the node to store it as Minimum Available.".into()
        )
    );
    let (_, c) = ctx();
    let out = run(&FundCheck, &c, json!({"threshold": 1000, "operator": "lt"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "Fund Check has a legacy 'lt' comparison, which this node cannot express - it only supports a minimum. Open the node and set Minimum Available."})
    );
    assert_eq!(out.condition_met(), None);
}

/// N-fundCheck-06: a minimum that does not resolve fails closed before
/// reading funds. The web read it as 0.0 and let any balance through
/// (fails on the web: its answer is condition True).
#[tokio::test]
async fn an_unreadable_minimum_never_passes_the_guard() {
    for d in [
        json!({"minAvailable": "{{typo}}"}),
        json!({"minAvailable": "abc"}),
        json!({"minAvailable": [1]}),
        json!({"minAvailable": ""}),
        json!({"minAvailable": null}),
        json!({"threshold": "{{typo}}"}),
    ] {
        let (client, c) = ctx();
        client.reply("funds", funds(json!(250000.5)));
        let out = run(&FundCheck, &c, d.clone()).await;
        assert_eq!(out.condition_met(), Some(false), "{}", d);
        assert!(out.is_error(), "{}", d);
        assert!(client.calls().is_empty(), "{}", d);
    }
    let (client, c) = ctx();
    client.reply("funds", funds(json!(250000.5)));
    let out = run(&FundCheck, &c, json!({"minAvailable": "{{typo}}"})).await;
    assert_eq!(
        out.failure_message(),
        "Fund Check's minimum did not resolve to a number: '{{typo}}'. Open the node and set Minimum Available."
    );
}

fn quote(data: Value) -> Value {
    json!({"status": "success", "data": data})
}

async fn price(d: Value, q: Value) -> (NodeOutcome, Vec<String>, usize) {
    let (client, c) = ctx();
    client.reply("quotes", q);
    let out = run(&PriceCondition, &c, d).await;
    let calls = client.calls_to("quotes").len();
    (out, messages(&c), calls)
}

/// X-201: an unknown field or operator, or an unreadable value, fails
/// rather than guessing (and reads no quote).
#[tokio::test]
async fn price_condition_fails_rather_than_guessing() {
    let q = quote(json!({"ltp": 100.0}));
    let base =
        json!({"symbol": "S", "exchange": "NSE", "field": "ltp", "operator": "<", "value": 100});
    for (key, bad, message) in [
        (
            "field",
            json!("close"),
            "field 'close' is not one of change_percent, high, low, ltp, open, prev_close",
        ),
        (
            "operator",
            json!("~="),
            "operator '~=' is not one of !=, <, <=, ==, >, >=, eq, gt, gte, lt, lte, neq",
        ),
        (
            "value",
            json!("abc"),
            "value did not resolve to a number: 'abc'",
        ),
        (
            "value",
            json!(true),
            "value did not resolve to a number: True",
        ),
    ] {
        let mut d = base.clone();
        d[key] = bad;
        let (out, logs, calls) = price(d, q.clone()).await;
        assert_eq!(
            answer(&out),
            json!({"status": "error", "condition": false, "message": message})
        );
        assert_eq!(logs, vec![format!("Price check aborted: {}", message)]);
        assert_eq!(calls, 0);
    }
}

/// X-202, X-203: valid configurations still evaluate.
#[tokio::test]
async fn price_condition_still_evaluates() {
    let q = quote(json!({"ltp": 100.0}));
    for (op, value, met) in [
        (">", 50, true),
        ("<", 50, false),
        (">=", 100, true),
        ("!=", 100, false),
        ("lte", 100, true),
    ] {
        let d = json!({"symbol": "S", "field": "ltp", "operator": op, "value": value});
        let (out, _, _) = price(d, q.clone()).await;
        assert_eq!(out.condition_met(), Some(met), "{} {}", op, value);
    }
    let d = json!({"symbol": "S", "field": "change_percent", "operator": ">", "value": 5});
    let (out, logs, _) = price(d, quote(json!({"ltp": 110.0, "prev_close": 100.0}))).await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "condition": true, "ltp": 110.0,
               "field": "change_percent", "field_value": 10.0})
    );
    assert_eq!(logs[1], "Price check: change_percent=10.0 > 5.0 = True");
    // The legacy threshold, and an empty value reading 0.
    let d = json!({"symbol": "S", "field": "ltp", "threshold": 900});
    assert_eq!(price(d, q.clone()).await.0.condition_met(), Some(false));
    let d = json!({"symbol": "S", "field": "ltp", "value": ""});
    assert_eq!(price(d, q).await.0.condition_met(), Some(true));
}

/// E-47, X-096: a failed quote surfaces the broker's text.
#[tokio::test]
async fn price_condition_refuses_on_a_failed_quote() {
    let d =
        json!({"symbol": "S", "exchange": "NSE", "field": "ltp", "operator": "<", "value": 100});
    let failed = json!({"status": "error", "error": "Broker session expired", "code": 401});
    let (out, _, _) = price(d, failed).await;
    assert!(out.is_error());
    assert_eq!(out.condition_met(), Some(false));
    assert!(out
        .failure_message()
        .to_lowercase()
        .contains("session expired"));
}

/// N-priceCondition-05: a quote without the field (or the previous close)
/// fails closed. The web evaluated on 0.0 with a success status (fails on
/// the web: `high=0.0 > 800.0 = False` takes the false branch).
#[tokio::test]
async fn a_quote_missing_the_field_fails_closed() {
    let q = quote(json!({"ltp": 812.5}));
    let d = json!({"symbol": "S", "field": "high", "operator": ">", "value": 800});
    let (out, logs, _) = price(d, q.clone()).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "condition": false, "message": "the quote has no high"})
    );
    assert_eq!(logs[1], "Price check aborted: the quote has no high");
    let d = json!({"symbol": "S", "field": "change_percent", "operator": ">", "value": 1});
    let (out, _, _) = price(d.clone(), q).await;
    assert_eq!(out.failure_message(), "the quote has no prev_close");
    let (out, _, _) = price(d, quote(json!({"ltp": 812.5, "prev_close": 0}))).await;
    assert_eq!(out.failure_message(), "the quote has no prev_close");
}

async fn var(d: Value) -> (NodeOutcome, Vec<(String, String)>) {
    let (_, c) = ctx();
    c.vars.set("rsi", json!({"latest": {"value": 27.4}}));
    c.vars.set("pdh", json!({"pdh": 818}));
    let out = run(&VarCondition, &c, d).await;
    (out, logs(&c))
}

#[tokio::test]
async fn var_condition_compares_resolved_numbers() {
    let (out, logs) =
        var(json!({"leftValue": "{{rsi.latest.value}}", "operator": "<", "rightValue": "30"}))
            .await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "condition": true, "left": 27.4, "operator": "<", "right": 30.0})
    );
    assert_eq!(logs[0].1, "Var check: 27.4 < 30.0 = True");
    let (out, _) = var(json!({"leftValue": 0, "operator": "==", "rightValue": "0.0"})).await;
    assert_eq!(out.condition_met(), Some(true));
    let (out, logs) =
        var(json!({"leftValue": " 1e2 ", "operator": "", "rightValue": "{{pdh.pdh}}"})).await;
    assert_eq!(answer(&out)["operator"], ">");
    assert_eq!(logs[0].1, "Var check: 100.0 > 818.0 = False");
}

/// N-varCondition-02/03/05 (qa 1687-1720): an operand or operator it
/// cannot compare takes neither branch.
#[tokio::test]
async fn var_condition_refuses_what_it_cannot_compare() {
    for (d, message) in [
        (
            json!({"leftValue": "", "rightValue": "1"}),
            "leftValue is empty",
        ),
        (
            json!({"leftValue": null, "rightValue": "1"}),
            "leftValue is empty",
        ),
        (
            json!({"leftValue": "high", "rightValue": "1"}),
            "leftValue did not resolve to a number: 'high'",
        ),
        (
            json!({"leftValue": "1", "rightValue": "{{threshold}}"}),
            "rightValue did not resolve to a number: '{{threshold}}'",
        ),
        (
            json!({"leftValue": "1", "rightValue": "2", "operator": "~"}),
            "operator '~' is not one of !=, <, <=, ==, >, >=, eq, gt, gte, lt, lte, neq",
        ),
    ] {
        let (out, logs) = var(d.clone()).await;
        assert_eq!(
            answer(&out),
            json!({"status": "error", "condition": false, "message": message}),
            "{}",
            d
        );
        assert_eq!(
            logs,
            vec![("error".into(), format!("Var check aborted: {}", message))]
        );
    }
}

#[test]
fn comparisons_take_symbols_and_words() {
    for (op, want) in [
        (">", false),
        ("gt", false),
        ("<", true),
        ("lt", true),
        ("==", false),
        ("eq", false),
        ("!=", true),
        ("neq", true),
        (">=", false),
        ("gte", false),
        ("<=", true),
        ("lte", true),
        ("~", false),
    ] {
        assert_eq!(compare(1.0, op, 2.0), want, "{}", op);
    }
}
