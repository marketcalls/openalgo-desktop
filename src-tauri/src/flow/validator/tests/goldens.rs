//! Every answer the web's own validator and node contracts gave, field for
//! field and in order, over the corpus in
//! `scripts/gen_flow_validator_goldens.py` (recorded from OpenAlgo web
//! 96e5927e5 into `tests/fixtures/flow/validator_goldens.json`). The corpus
//! reaches every rule, quotes awkward values in every message, and includes
//! every JSON example in the web's `docs/prompt/flow-import-format.md`.
//! Cases where the desktop deliberately differs are left out of the fixture
//! and pinned in `rules`.

use crate::flow::node_contracts as contracts;
use crate::flow::validator::{
    migrate_legacy_node_data, trigger_config, validate_workflow, ValidateOptions,
};
use chrono::NaiveDate;
use serde_json::Value;
use std::sync::LazyLock;

static GOLDENS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/fixtures/flow/validator_goldens.json"
    ))
    .unwrap()
});

fn section(name: &str) -> &'static [Value] {
    GOLDENS[name].as_array().unwrap()
}

/// One recorded validation case by name: its payload and, per level, the
/// web's answer.
pub(super) fn web_case(name: &str) -> &'static Value {
    section("validate")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no golden case {name}"))
}

/// The desktop's answer for a recorded case at a level (`complete` or
/// `draft`), after checking it is the web's recorded answer.
pub(super) fn checked_answer(
    name: &str,
    level: &str,
) -> Vec<crate::flow::validator::ValidationError> {
    let case = web_case(name);
    let errors = validate_workflow(&case["payload"], self::level(level));
    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        case["levels"][level]["errors"],
        "{name} ({level}) differs from the web"
    );
    errors
}

/// The web's recorded `TRIGGER_CONFIG_FIELDS`, in its order.
pub(super) fn web_trigger_config_fields() -> Vec<&'static str> {
    GOLDENS["trigger_config_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect()
}

fn level(name: &str) -> ValidateOptions {
    match name {
        "complete" => ValidateOptions::COMPLETE,
        "draft" => ValidateOptions::DRAFT,
        "unnamed" => ValidateOptions {
            require_name: false,
            strict: true,
        },
        other => panic!("unknown level {other}"),
    }
}

/// The first difference between two error lists, for a readable failure.
fn first_difference(want: &[Value], got: &[Value]) -> String {
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        if w != g {
            return format!("error {i}:\n  web     {w}\n  desktop {g}");
        }
    }
    format!(
        "lengths differ: web {} desktop {}\n  web     {:?}\n  desktop {:?}",
        want.len(),
        got.len(),
        want.get(got.len()),
        got.get(want.len())
    )
}

#[test]
fn every_error_is_the_webs_error_in_the_webs_order() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for case in section("validate") {
        let name = case["name"].as_str().unwrap();
        for (level_name, result) in case["levels"].as_object().unwrap() {
            let got = validate_workflow(&case["payload"], level(level_name));
            let got = serde_json::to_value(&got).unwrap();
            let got = got.as_array().unwrap();
            if let Some(raised) = result.get("raises") {
                // The web answers a 500 here; the desktop answers errors.
                assert!(
                    !got.is_empty(),
                    "{name} ({level_name}) raised {raised} on the web"
                );
                continue;
            }
            checked += 1;
            let want = result["errors"].as_array().unwrap();
            if want != got {
                failures.push(format!(
                    "{name} ({level_name}): {}",
                    first_difference(want, got)
                ));
            }
        }
    }
    assert!(checked > 3000, "only {checked} cases checked");
    assert!(
        failures.is_empty(),
        "{} of {checked} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn every_web_doc_example_was_recorded_valid() {
    let examples: Vec<&Value> = section("validate")
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("doc_fence_"))
        .collect();
    assert!(examples.len() > 30, "{} doc examples", examples.len());
    for case in examples {
        assert_eq!(
            case["levels"]["complete"]["errors"],
            serde_json::json!([]),
            "{}",
            case["name"]
        );
    }
}

/// V-102, V-103, V-104: what the web's validator leaves to the run (the
/// httpRequest address, which the node's outbound guard checks; option
/// expiries, offsets and strategy names; the presence-only fields such as
/// a mathExpression, an operator, a time or a basket row) passes validation
/// here as it does there. Adding a check for any of them fails this test.
#[test]
fn fields_left_to_the_run_pass_validation_as_on_the_web() {
    let cases: Vec<&Value> = section("validate")
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("runtime_"))
        .collect();
    assert!(cases.len() >= 28, "{} runtime cases", cases.len());
    for case in cases {
        let recorded = &case["levels"]["complete"]["errors"];
        assert_eq!(recorded, &serde_json::json!([]), "{}", case["name"]);
        let errors = validate_workflow(&case["payload"], ValidateOptions::COMPLETE);
        assert_eq!(errors, vec![], "{}", case["name"]);
    }
}

#[test]
fn legacy_nodes_migrate_as_the_web_migrates_them() {
    for case in section("migrate") {
        let nodes = case["nodes"].as_array().unwrap().clone();
        let (migrated, notes) = migrate_legacy_node_data(nodes);
        assert_eq!(Value::from(migrated), case["migrated"], "{}", case["name"]);
        assert_eq!(Value::from(notes), case["notes"], "{}", case["name"]);
    }
}

#[test]
fn trigger_configuration_is_the_webs() {
    for case in section("trigger_config") {
        let config = trigger_config(case["nodes"].as_array().unwrap());
        assert_eq!(
            serde_json::to_value(&config).unwrap(),
            case["config"],
            "{}",
            case["name"]
        );
    }
    for case in section("trigger_config_equal") {
        let before = trigger_config(case["before"].as_array().unwrap());
        let after = trigger_config(case["after"].as_array().unwrap());
        assert_eq!(before == after, case["equal"].as_bool().unwrap(), "{case}");
    }
}

fn contract_cases(name: &str) -> &'static [Value] {
    GOLDENS["contracts"][name].as_array().unwrap()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap()
}

#[test]
fn expiry_selection_is_the_webs() {
    for case in contract_cases("select_expiry") {
        let listed: Vec<&str> = case["expiries"]
            .as_array()
            .unwrap()
            .iter()
            .map(text)
            .collect();
        let today = NaiveDate::parse_from_str(text(&case["today"]), "%Y-%m-%d").unwrap();
        let got = contracts::select_expiry(&listed, text(&case["type"]), today);
        assert_eq!(got.as_deref(), case["result"].as_str(), "{case}");
    }
}

#[test]
fn expiry_parsing_is_the_webs() {
    for case in contract_cases("parse_expiry_date") {
        let raw = text(&case["value"]);
        let got = contracts::parse_expiry_date(raw).map(|d| d.format("%Y-%m-%d").to_string());
        assert_eq!(got.as_deref(), case["date"].as_str(), "{raw:?}");
        assert_eq!(
            contracts::format_expiry_for_api(raw),
            text(&case["api"]),
            "{raw:?}"
        );
    }
}

#[test]
fn the_string_contracts_are_the_webs() {
    for case in contract_cases("parse_underlying_symbol") {
        let (base, expiry) = contracts::parse_underlying_symbol(text(&case["value"]));
        assert_eq!(serde_json::json!([base, expiry]), case["result"], "{case}");
    }
    for case in contract_cases("default_product_for_exchange") {
        let got = contracts::default_product_for_exchange(case["value"].as_str());
        assert_eq!(got, text(&case["result"]), "{case}");
    }
    for case in contract_cases("normalize_status") {
        assert_eq!(
            contracts::normalize_status(text(&case["value"])),
            text(&case["result"]),
            "{case}"
        );
    }
    for case in contract_cases("normalize_price_condition") {
        assert_eq!(
            contracts::normalize_condition(text(&case["value"])),
            text(&case["result"]),
            "{case}"
        );
    }
    for case in contract_cases("is_option_offset") {
        let got = contracts::is_option_offset(text(&case["value"]));
        assert_eq!(got, case["result"].as_bool().unwrap(), "{case}");
    }
    for case in contract_cases("is_expiry_date") {
        let got = contracts::is_expiry_date(text(&case["value"]));
        assert_eq!(got, case["result"].as_bool().unwrap(), "{case}");
    }
}

#[test]
fn the_contract_vocabularies_are_the_webs() {
    let as_list = |items: &[&str]| Value::from(items.to_vec());
    let web = &GOLDENS["contracts"];
    assert_eq!(as_list(&contracts::VALID_STATUSES), web["valid_statuses"]);
    assert_eq!(
        as_list(&contracts::VALID_EXPIRY_TYPES),
        web["valid_expiry_types"]
    );
    assert_eq!(
        as_list(&contracts::VALID_LEG_STRIKE_MODES),
        web["valid_leg_strike_modes"]
    );
    assert_eq!(
        as_list(&contracts::DERIVATIVE_EXCHANGES),
        web["derivative_exchanges"]
    );
}
