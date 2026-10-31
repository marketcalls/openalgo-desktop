//! Branches of the web's rules that its own tests and the main golden
//! families do not reach, each named over a case recorded from the web
//! (the answer is checked against the web's before the branch is asserted).

use super::goldens::{checked_answer, web_case};
use super::*;

/// V-006: an AND or OR gate whose id is an array makes the web's validator
/// raise (its gate-slot dict cannot hold an unhashable key, a 500 there);
/// here it is reported as the bad id it is and the rest is checked.
#[test]
fn v006_a_gate_with_an_array_id_is_reported_not_raised() {
    for gate in ["andGate", "orGate"] {
        let case = web_case(&format!("crash_array_{gate}_id"));
        assert_eq!(case["levels"]["draft"]["raises"], "TypeError", "{gate}");
        let errors = draft(&case["payload"]);
        assert_eq!(
            errors
                .iter()
                .map(|e| (e.path.as_str(), e.message.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("/nodes/1/id", "Node needs a non-empty string id"),
                (
                    "/edges/0/targetHandle",
                    "Only gate nodes have numbered input slots; a start node has no 'input-1'."
                ),
            ],
            "{gate}"
        );
    }
}

/// V-008: a value is a template, and left to the run, whenever its text
/// holds `{{`, closed or not (`{{x`, `a{{`), not only a full `{{...}}`.
#[test]
fn v008_any_text_holding_double_braces_is_left_to_the_run() {
    for i in 0..10 {
        let name = format!("branch_open_template_{i}");
        let draft_errors = checked_answer(&name, "draft");
        assert_eq!(draft_errors, vec![], "{name}");
        checked_answer(&name, "complete");
    }
    // An orderId holding "{{" is the one place a template is refused.
    let errors = checked_answer("branch_open_template_7", "complete");
    assert_eq!(errors[0].code, "invalid_trigger_filter");
}

/// V-034: the size limits do not stop validation; the nodes and edges past
/// them are still checked.
#[test]
fn v034_validation_goes_on_past_the_size_limits() {
    let errors = checked_answer("error_order_every_pass", "complete");
    assert_eq!(codes(&errors[..2]), vec!["too_large", "too_large"]);
    assert!(errors.len() > 2 && errors[2].path == "/nodes/0/id");
}

/// V-058: a generated strategy's legs are never checked, and a bad price
/// type is the only error its own check returns.
#[test]
fn v058_a_generated_strategy_checks_only_its_price_type() {
    for i in 0..2 {
        for level in ["complete", "draft"] {
            assert_eq!(
                checked_answer(&format!("branch_generated_legs_{i}"), level),
                vec![]
            );
        }
    }
    for level in ["complete", "draft"] {
        let errors = checked_answer("branch_generated_legs_2", level);
        assert_eq!(
            errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>(),
            vec!["Generated option strategies support only MARKET and LIMIT price types."]
        );
    }
}

/// V-062: a leg naming a strike is checked on the strike alone (its offset,
/// even an impossible one, is not read); a blank or null strike falls back
/// to the offset; strikeMode does not choose between them.
#[test]
fn v062_a_named_strike_wins_over_the_offset() {
    let at = |i: usize| checked_answer(&format!("branch_strike_over_offset_{i}"), "complete");
    assert_eq!(at(0), vec![]);
    assert_eq!(codes(&at(1)), vec!["invalid_strike"]);
    let fallback = at(2);
    assert_eq!(fallback[0].message, "'OTM99' is not a valid offset.");
    assert_eq!(at(3), vec![]);
    assert_eq!(
        at(4),
        vec![],
        "strikeMode OFFSET still reads the strike first"
    );
}

/// V-071: a templated symbol is a watchable filter; only a templated
/// orderId is refused.
#[test]
fn v071_a_templated_symbol_is_not_refused() {
    for i in 0..2 {
        for level in ["complete", "draft"] {
            assert_eq!(
                checked_answer(&format!("branch_templated_symbol_{i}"), level),
                vec![]
            );
        }
    }
}

/// V-076: a save (draft) runs none of priceAlert's condition and threshold
/// checks, which activation (complete) does.
#[test]
fn v076_a_save_runs_no_price_alert_condition_checks() {
    for i in 0..5 {
        let name = format!("branch_alert_on_save_{i}");
        assert_eq!(checked_answer(&name, "draft"), vec![], "{name}");
        assert!(!checked_answer(&name, "complete").is_empty(), "{name}");
    }
}

/// V-101: the schedule fields of a start node and a webhook trigger's fields
/// are not validated here (activation and the scheduler read them).
#[test]
fn v101_trigger_fields_are_left_to_activation() {
    for name in ["runtime_trigger_start", "runtime_trigger_webhook"] {
        assert_eq!(checked_answer(name, "complete"), vec![], "{name}");
    }
}
