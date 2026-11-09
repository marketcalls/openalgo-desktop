//! The approval registry, `tests/fixtures/feed_corpus/approvals.json`: the
//! lead's and the maintainer's rules a deviation follows, each quoted from
//! the lane record with who decided it, when, and where it is written. A
//! deviation's `"approval"` names one; the harness checks that it does, and
//! that it is not a rule a later decision withdrew (`superseded_by`); the
//! release gate (`scripts/feed_corpus/release_gate.py`) fails on any
//! deviation without one.

use super::case::corpus_root;
use super::json_text;
use serde_json::{Map, Value};
use std::sync::OnceLock;

/// The fields every rule has.
const RULE_FIELDS: &[&str] = &["by", "date", "source", "rule"];
/// The rule that replaced a withdrawn one.
const SUPERSEDED_BY: &str = "superseded_by";
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

/// The rule that replaced `id`, if a later decision withdrew it.
fn superseded_by(id: &str) -> Option<&'static str> {
    rules().get(id)?.get(SUPERSEDED_BY)?.as_str()
}

/// Why a deviation's `approval` is not one it may cite, if it is not: it
/// names no rule, or a rule a later decision withdrew.
pub(crate) fn problem(approval: &Value) -> Option<String> {
    let Some(r) = approval.as_str() else {
        return Some("\"approval\" is the name of a rule in approvals.json".into());
    };
    if !rules().contains_key(r) {
        return Some(format!("approval \"{}\" is no rule in approvals.json", r));
    }
    superseded_by(r).map(|newer| {
        format!(
            "approval \"{}\" cites a withdrawn rule: \"{}\" replaced it",
            r, newer
        )
    })
}

/// Every rule that is not complete: each says who decided it (the lead or
/// the maintainer), when, where it is written, and quotes it; a withdrawn
/// one names the rule in force that replaced it.
pub(crate) fn registry_problems() -> Vec<String> {
    let mut out = Vec::new();
    for (id, rule) in rules() {
        let Some(o) = rule.as_object() else {
            out.push(format!("rule {} is not an object", id));
            continue;
        };
        for k in o
            .keys()
            .filter(|k| !RULE_FIELDS.contains(&k.as_str()) && k.as_str() != SUPERSEDED_BY)
        {
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
        if let Some(newer) = o.get(SUPERSEDED_BY) {
            let in_force = newer
                .as_str()
                .is_some_and(|n| rules().contains_key(n) && superseded_by(n).is_none());
            if !in_force {
                out.push(format!(
                    "rule {}: \"{}\" names a rule in force",
                    id, SUPERSEDED_BY
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_approval_names_a_rule_in_force() {
        assert_eq!(problem(&json!("leave-out-unsent")), None);
        assert!(problem(&json!("my-own-idea")).is_some_and(|p| p.contains("no rule")));
        assert!(problem(&json!(true)).is_some());
        let withdrawn = problem(&json!("per-packet-only-persistent-extras"));
        assert!(
            withdrawn
                .as_deref()
                .is_some_and(|p| p.contains("market-state-overlay")),
            "{:?}",
            withdrawn
        );
    }
}
