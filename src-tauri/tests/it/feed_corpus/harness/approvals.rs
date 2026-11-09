//! The approval registry, `tests/fixtures/feed_corpus/approvals.json`: the
//! lead's and the maintainer's rules a deviation follows, each quoted from
//! the lane record with who decided it, when, and where it is written. A
//! deviation's `"approval"` names one; the harness checks that it does, that
//! it is not a rule a later decision withdrew (`superseded_by`), and that the
//! deviation is one the rule's words cover (`scope`: which brokers, keys,
//! kinds, instruments or steps; written from the decision, never widened to
//! fit a case); the release gate (`scripts/feed_corpus/release_gate.py`) fails
//! on any deviation without one.

use super::case::{corpus_root, CaseFacts};
use super::json_text;
use serde_json::{Map, Value};
use std::sync::OnceLock;

/// The fields every rule has.
const RULE_FIELDS: &[&str] = &["by", "date", "source", "rule"];
/// The rule that replaced a withdrawn one.
const SUPERSEDED_BY: &str = "superseded_by";
/// What a decision applies to, in its own words.
const SCOPE: &str = "scope";
/// The fields of a scope's clause, and `any_of` of several clauses.
const SCOPE_FIELDS: &[&str] = &[
    "brokers",
    "keys",
    "kinds",
    "allows",
    "instrument",
    "after_step",
];
const ANY_OF: &str = "any_of";
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

/// A scope's list item: an exact name, or a prefix ending in `*`.
fn pattern_matches(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

/// Is this deviation within one clause of a rule's scope?
fn clause_fits(clause: &Value, facts: &CaseFacts, key: &str, kind: &str, allows: &[Value]) -> bool {
    let named = |field: &str, value: &str| {
        clause.get(field).and_then(Value::as_array).is_none_or(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .any(|p| pattern_matches(p, value))
        })
    };
    let instrument = match clause.get("instrument").and_then(Value::as_str) {
        Some("index") => facts.index,
        Some("non-index") => facts.non_index,
        _ => true,
    };
    let after_step = clause
        .get("after_step")
        .and_then(Value::as_str)
        .is_none_or(|s| facts.steps.contains(s));
    let only_these = clause
        .get("allows")
        .and_then(Value::as_array)
        .is_none_or(|l| allows.iter().all(|a| l.contains(a)));
    named("brokers", &facts.broker)
        && named("keys", key)
        && named("kinds", kind)
        && only_these
        && instrument
        && after_step
}

/// Is this deviation within the words of its rule?
fn in_scope(scope: &Value, facts: &CaseFacts, key: &str, kind: &str, allows: &[Value]) -> bool {
    match scope.get(ANY_OF).and_then(Value::as_array) {
        Some(clauses) => clauses
            .iter()
            .any(|c| clause_fits(c, facts, key, kind, allows)),
        None => clause_fits(scope, facts, key, kind, allows),
    }
}

/// Why a deviation (`key`, `kind`, of a case) may not cite `approval`, if it
/// may not: it names no rule, a rule a later decision withdrew, or a rule
/// whose words do not cover it.
pub(crate) fn problem(
    approval: &Value,
    facts: &CaseFacts,
    key: &str,
    kind: &str,
    allows: &[Value],
) -> Option<String> {
    let Some(r) = approval.as_str() else {
        return Some("\"approval\" is the name of a rule in approvals.json".into());
    };
    let Some(rule) = rules().get(r) else {
        return Some(format!("approval \"{}\" is no rule in approvals.json", r));
    };
    if let Some(newer) = superseded_by(r) {
        return Some(format!(
            "approval \"{}\" cites a withdrawn rule: \"{}\" replaced it",
            r, newer
        ));
    }
    let scope = rule.get(SCOPE).unwrap_or(&Value::Null);
    (!in_scope(scope, facts, key, kind, allows)).then(|| {
        format!(
            "rule \"{}\" does not cover this deviation (its scope: {})",
            r, scope
        )
    })
}

/// What is wrong with one clause of a rule's scope.
fn clause_problems(id: &str, clause: &Value) -> Vec<String> {
    let Some(o) = clause.as_object() else {
        return vec![format!("rule {}: a scope clause is an object", id)];
    };
    let mut out: Vec<String> = o
        .keys()
        .filter(|k| !SCOPE_FIELDS.contains(&k.as_str()))
        .map(|k| format!("rule {}: \"{}\" is not a field of a scope", id, k))
        .collect();
    for field in ["brokers", "keys", "kinds", "allows"] {
        let bad_list = o.get(field).is_some_and(|v| {
            !v.as_array()
                .is_some_and(|l| !l.is_empty() && l.iter().all(Value::is_string))
        });
        if bad_list {
            out.push(format!("rule {}: \"{}\" is a list of names", id, field));
        }
    }
    if o.get("instrument")
        .is_some_and(|v| v != "index" && v != "non-index")
    {
        out.push(format!(
            "rule {}: \"instrument\" is \"index\" or \"non-index\"",
            id
        ));
    }
    if o.get("after_step").is_some_and(|v| !v.is_string()) {
        out.push(format!("rule {}: \"after_step\" names a step", id));
    }
    out
}

/// What is wrong with a rule's `scope`: every rule has one (an empty one
/// covers everything, a statement about the decision's words).
fn scope_problems(id: &str, scope: Option<&Value>) -> Vec<String> {
    let Some(scope) = scope else {
        return vec![format!("rule {}: no \"{}\"", id, SCOPE)];
    };
    match scope.get(ANY_OF) {
        Some(Value::Array(clauses)) if !clauses.is_empty() => clauses
            .iter()
            .flat_map(|c| clause_problems(id, c))
            .chain(
                scope
                    .as_object()
                    .filter(|o| o.len() > 1)
                    .map(|_| format!("rule {}: {} stands alone", id, ANY_OF)),
            )
            .collect(),
        Some(_) => vec![format!(
            "rule {}: \"{}\" is a non-empty list of clauses",
            id, ANY_OF
        )],
        None => clause_problems(id, scope),
    }
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
        for k in o.keys().filter(|k| {
            !RULE_FIELDS.contains(&k.as_str()) && ![SUPERSEDED_BY, SCOPE].contains(&k.as_str())
        }) {
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
        out.extend(scope_problems(id, o.get(SCOPE)));
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
        let facts = CaseFacts::new("zerodha", None, "");
        let check = |rule: Value| problem(&rule, &facts, "ltp", "web-defect", &[]);
        assert_eq!(check(json!("leave-out-unsent")), None);
        assert!(check(json!("my-own-idea")).is_some_and(|p| p.contains("no rule")));
        assert!(check(json!(true)).is_some());
        let withdrawn = check(json!("per-packet-only-persistent-extras"));
        assert!(
            withdrawn
                .as_deref()
                .is_some_and(|p| p.contains("market-state-overlay")),
            "{:?}",
            withdrawn
        );
    }

    /// Review 2, O5: a rule covers only what its decision's words cover: an
    /// index-only rule on an equity case, or a broker's rule on another
    /// broker, is named.
    #[test]
    fn a_deviation_outside_its_rules_words_is_named() {
        let equity = CaseFacts::new(
            "hdfcsky",
            Some(&json!([{"symbol": "RELIANCE", "exchange": "NSE"}])),
            "",
        );
        let index = CaseFacts::new(
            "hdfcsky",
            Some(&json!([{"symbol": "NIFTY", "exchange": "NSE_INDEX"}])),
            "",
        );
        let rule = json!("no-packet-time-as-trade-time");
        assert_eq!(problem(&rule, &index, "ltt", "web-defect", &[]), None);
        let out_of_words = problem(&rule, &equity, "ltt", "web-defect", &[]);
        assert!(
            out_of_words
                .as_deref()
                .is_some_and(|p| p.contains("does not cover")),
            "{:?}",
            out_of_words
        );
        assert!(problem(&rule, &index, "ltp", "web-defect", &[]).is_some());
        assert!(problem(&rule, &index, "ltt", "desktop-decision", &[]).is_some());
        let dhan = json!("dhan-f32-shortest-decimal");
        assert!(problem(&dhan, &equity, "ltp", "web-defect", &[]).is_some());
    }

    /// The lead's ruling on `do-not-invent-data`: under it a `change` must be
    /// the one computed from the invented zero, never a real change value.
    #[test]
    fn a_rule_can_fix_what_a_deviation_allows() {
        let facts = CaseFacts::new("hdfcsky", None, "");
        let rule = json!("do-not-invent-data");
        let derived = [json!("change-from-zero-ltp")];
        assert_eq!(
            problem(&rule, &facts, "change", "web-defect", &derived),
            None
        );
        let a_real_value = [json!([-5.0, "<absent>"])];
        assert!(problem(&rule, &facts, "change", "web-defect", &a_real_value).is_some());
    }
}
