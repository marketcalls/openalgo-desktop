//! The order the web reports errors in, which callers rely on (a route puts
//! `errors[0].message` in front of the trader): every pass of the web's
//! `validate_workflow` in one payload, the cycle search's edge order, one
//! cycle error at most, the enum fields' order, and the trigger
//! configuration's field order. Each case is also a golden recorded from the
//! web, so these name what the goldens compare.

use super::goldens::{web_case, web_trigger_config_fields};
use super::*;
use crate::flow::validator::TRIGGER_CONFIG_FIELDS;

/// `(path, code)` of every error, in order.
fn order(errors: &[ValidationError]) -> Vec<(String, &'static str)> {
    errors.iter().map(|e| (e.path.clone(), e.code)).collect()
}

/// The desktop's answer for a recorded case, checked against the web's.
fn answer(name: &str, level: &str) -> Vec<ValidationError> {
    let case = web_case(name);
    let errors = if level == "draft" {
        draft(&case["payload"])
    } else {
        strict(&case["payload"])
    };
    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        case["levels"][level]["errors"],
        "{name} ({level}) differs from the web"
    );
    errors
}

fn pairs(list: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    list.iter().map(|(p, c)| (p.to_string(), *c)).collect()
}

/// V-004 (and V-034): both size limits first (nodes, then edges), then each
/// node's id, type, data and position, then every edge's id, source and
/// target, then all source handles, then all target handles, then the
/// trigger count; validation goes on past the size limits.
#[test]
fn v004_errors_come_in_the_webs_pass_order() {
    assert_eq!(
        order(&answer("error_order_every_pass", "complete")),
        pairs(&[
            ("/nodes", "too_large"),
            ("/edges", "too_large"),
            ("/nodes/0/id", "required"),
            ("/nodes/0/type", "required"),
            ("/nodes/0/data", "required"),
            ("/nodes/0/position", "required"),
            ("/edges/0/id", "required"),
            ("/edges/0/source", "required"),
            ("/edges/0/target", "required"),
            ("/edges/3/id", "duplicate"),
            ("/edges/3/source", "dangling_edge"),
            ("/edges/2/sourceHandle", "invalid_source_handle"),
            ("/edges/1/targetHandle", "invalid_target_handle"),
            ("/nodes", "no_trigger"),
        ])
    );
}

/// V-005: the cycle search follows a node's edges in the order they are
/// listed, so which of two cycles reachable from one node is reported
/// depends on that order.
#[test]
fn v005_the_cycle_search_follows_edges_in_listed_order() {
    let reported = |name: &str| {
        let errors = answer(name, "complete");
        errors[0].message.rsplit(": ").next().unwrap().to_string()
    };
    assert_eq!(reported("error_order_cycle_bc_first"), "b -> c -> b");
    assert_eq!(reported("error_order_cycle_de_first"), "d -> e -> d");
}

/// V-094: one cycle error at most, however many cycles; unreachable nodes
/// are reported after it.
#[test]
fn v094_one_cycle_error_however_many_cycles() {
    let errors = answer("error_order_two_disjoint_cycles", "complete");
    assert_eq!(codes(&errors), vec!["cycle", "unreachable"]);
    assert!(errors[0].message.ends_with(": x -> y -> x"));
}

/// V-019: the enum fields in the web's ENUM_FIELDS order, then the
/// positive-number fields, at both levels.
#[test]
fn v019_enum_fields_are_checked_in_the_webs_order() {
    let want = pairs(&[
        ("/nodes/1/data/exchange", "invalid_constant"),
        ("/nodes/1/data/action", "invalid_constant"),
        ("/nodes/1/data/product", "invalid_constant"),
        ("/nodes/1/data/priceType", "invalid_constant"),
        ("/nodes/1/data/pricetype", "invalid_constant"),
        ("/nodes/1/data/operation", "invalid_constant"),
        ("/nodes/1/data/quantity", "invalid_quantity"),
    ]);
    for level in ["complete", "draft"] {
        assert_eq!(
            order(&answer("error_order_enum_fields", level)),
            want,
            "{level}"
        );
    }
}

/// V-120: the trigger configuration's fields, all 19, in the web's order,
/// which the Agent's `validate_flow` result follows.
#[test]
fn v120_trigger_config_fields_are_the_webs_in_order() {
    assert_eq!(TRIGGER_CONFIG_FIELDS.to_vec(), web_trigger_config_fields());
}
