//! The approval registry, `tests/fixtures/feed_corpus/approvals.json`: the
//! lead's and the maintainer's rules a deviation follows, each quoted from
//! the lane record with who decided it, when, and where it is written. A
//! deviation's `"approval"` names one; the harness checks that it does, and
//! the release gate (`scripts/feed_corpus/release_gate.py`) fails on any
//! deviation without one.

use super::case::corpus_root;
use super::json_text;
use serde_json::{Map, Value};
use std::sync::OnceLock;

/// The fields a rule has.
const RULE_FIELDS: &[&str] = &["by", "date", "source", "rule"];
/// Who may approve.
const DECIDERS: &[&str] = &["lead", "maintainer"];

fn rules() -> &'static Map<String, Value> {
    static RULES: OnceLock<Map<String, Value>> = OnceLock::new();
    RULES.get_or_init(|| {
        let path = corpus_root().join("approvals.json");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
        let doc = json_text::parse(&text)
            .unwrap_or_else(|e| panic!("{}: not JSON: {}", path.display(), e));
        doc.get("rules")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(|| panic!("{}: no \"rules\" object", path.display()))
    })
}

/// Why a deviation's `approval` names no rule, if it does not.
pub(crate) fn problem(approval: &Value) -> Option<String> {
    match approval.as_str() {
        None => Some("\"approval\" is the name of a rule in approvals.json".into()),
        Some(r) if !rules().contains_key(r) => {
            Some(format!("approval \"{}\" is no rule in approvals.json", r))
        }
        Some(_) => None,
    }
}

/// Every rule that is not complete: each says who decided it (the lead or
/// the maintainer), when, where it is written, and quotes it.
pub(crate) fn registry_problems() -> Vec<String> {
    let mut out = Vec::new();
    for (id, rule) in rules() {
        let Some(o) = rule.as_object() else {
            out.push(format!("rule {} is not an object", id));
            continue;
        };
        for k in o.keys().filter(|k| !RULE_FIELDS.contains(&k.as_str())) {
            out.push(format!("rule {}: \"{}\" is not a field of a rule", id, k));
        }
        for k in RULE_FIELDS {
            if !o
                .get(*k)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.trim().is_empty())
            {
                out.push(format!("rule {}: no \"{}\"", id, k));
            }
        }
        if !o
            .get("by")
            .and_then(Value::as_str)
            .is_some_and(|b| DECIDERS.contains(&b))
        {
            out.push(format!(
                "rule {}: \"by\" is one of {}",
                id,
                DECIDERS.join(", ")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_approval_names_a_rule_of_the_registry() {
        assert_eq!(problem(&json!("leave-out-unsent")), None);
        assert!(problem(&json!("my-own-idea")).is_some_and(|p| p.contains("no rule")));
        assert!(problem(&json!(true)).is_some());
    }
}
