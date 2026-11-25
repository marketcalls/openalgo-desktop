//! The "where to look for company figures" section against the web's own
//! rendering (`tests/fixtures/agent/web_sources.json`, recorded by
//! `scripts/agent/capture_web_sources.py`), and the web's
//! `test_agent_webfetch.py` prompt cases.

use crate::agent::websearch::prompt::section;
use serde_json::Value;

const FIXTURE: &str = include_str!("../../../../tests/fixtures/agent/web_sources.json");

fn sources(case: &Value) -> Vec<String> {
    case["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// Every recorded case, byte for byte.
#[test]
fn the_section_is_the_webs_byte_for_byte() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"].as_object().unwrap();
    assert_eq!(cases.len(), 4);
    for (name, case) in cases {
        let ours = section(&sources(case));
        match &case["section"] {
            Value::Null => assert!(ours.is_none(), "{}", name),
            web => {
                let ours = ours.unwrap_or_else(|| panic!("{}: no section", name));
                assert_eq!(ours.key, web["key"].as_str().unwrap(), "{}", name);
                assert_eq!(ours.title, web["title"].as_str().unwrap(), "{}", name);
                assert_eq!(ours.body, web["body"].as_str().unwrap(), "{}", name);
                assert_eq!(ours.order, web["order"].as_i64().unwrap(), "{}", name);
                assert_eq!(ours.pinned, web["pinned"].as_bool().unwrap(), "{}", name);
            }
        }
    }
}

/// Web `test_the_prompt_names_the_sources_in_order_and_the_failure_rule`.
#[test]
fn the_prompt_names_the_sources_in_order_and_the_failure_rule() {
    let s = section(&["screener.in".into(), "scanx.trade".into()]).unwrap();
    assert!(s.body.find("screener.in") < s.body.find("scanx.trade"));
    assert!(s.body.contains("that source is dropped") && s.body.contains("next one is searched"));
    assert!(s
        .body
        .contains("https://www.screener.in/company/RELIANCE/consolidated/"));
}

/// Web `test_the_direct_page_advice_is_only_given_when_screener_is_preferred`.
#[test]
fn the_direct_page_advice_is_only_given_when_screener_is_preferred() {
    let s = section(&["scanx.trade".into()]).unwrap();
    assert!(!s.body.contains("screener.in/company"));
}

/// Web `test_no_preferred_source_means_no_prompt_section`.
#[test]
fn no_preferred_source_means_no_prompt_section() {
    assert!(section(&[]).is_none());
}
