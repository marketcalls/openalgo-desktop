//! Finding an indicator by intent and reading its signature: the web's
//! `search_specs` and `spec_to_dict` (`services/agent/indicators/compute.py`)
//! and the bodies of `list_indicators` and `describe_indicator`
//! (`services/agent/tools/indicators.py`).

use serde_json::Value;

use super::compute::default_json;
use super::descriptions::describe;
use super::json::Json;
use super::registry::{self, IndicatorSpec, Status, CATEGORIES};
use crate::agent::contract::tool::ToolOutcome;
use crate::agent::tools::market::normalise::refuse;
use crate::agent::tools::values::py_repr;

/// Above this many matches `list_indicators` returns names grouped by
/// category instead of a description each (`LIST_DETAIL_LIMIT`).
pub(crate) const LIST_DETAIL_LIMIT: usize = 45;

fn strings(items: &[&str]) -> Json {
    Json::Arr(items.iter().map(|s| Json::str(*s)).collect())
}

/// `spec_to_dict`: the one-line form `list_indicators` returns, or with
/// `verbose` the full signature `describe_indicator` returns.
pub(crate) fn spec_to_dict(spec: &IndicatorSpec, verbose: bool) -> Json {
    let mut out = Json::obj([
        ("name", Json::str(spec.name)),
        ("category", Json::str(spec.category)),
        ("description", Json::str(describe(spec.name))),
    ]);
    if spec.status != Status::Ok {
        out.set("status", Json::str(spec.status.as_str()));
        if let Some(alias) = spec.alias_of {
            out.set("alias_of", Json::str(alias));
        }
    }
    if !verbose {
        return out;
    }
    let requires: &[&str] = if spec.inputs.is_empty() {
        &["two derived series"]
    } else {
        spec.inputs
    };
    out.set("requires_series", strings(requires));
    out.set("outputs", strings(spec.outputs));
    out.set("warmup_bars", Json::Int(spec.warmup as i64));
    let parameters = spec.params.iter().map(|p| {
        let mut entry = Json::obj([
            ("type", Json::str(p.kind.as_str())),
            ("default", default_json(p.default)),
        ]);
        if p.required() {
            entry.set("required", Json::Bool(true));
        }
        if !p.allowed.is_empty() {
            entry.set("allowed", strings(p.allowed));
        }
        (p.name, entry)
    });
    out.set("parameters", Json::obj(parameters));
    if spec.needs_second_series {
        out.set("needs_second_symbol", Json::Bool(true));
    }
    if !spec.conditional_outputs.is_empty() {
        let conditional = spec
            .conditional_outputs
            .iter()
            .map(|(condition, outputs)| (*condition, strings(outputs)));
        out.set("conditional_outputs", Json::obj(conditional));
    }
    if !spec.note.is_empty() {
        out.set("note", Json::str(spec.note));
    }
    out
}

/// `search_specs`: a case-insensitive substring of the name, category,
/// description and note, optionally within one category; sorted by
/// category, then name.
pub(crate) fn search_specs(query: &str, category: &str) -> Vec<&'static IndicatorSpec> {
    let needle = query.trim().to_lowercase();
    let wanted = category.trim().to_lowercase();
    let mut found: Vec<&'static IndicatorSpec> = registry::all()
        .filter(|spec| wanted.is_empty() || spec.category.to_lowercase() == wanted)
        .filter(|spec| {
            needle.is_empty()
                || format!(
                    "{} {} {} {}",
                    spec.name,
                    spec.category,
                    describe(spec.name),
                    spec.note
                )
                .to_lowercase()
                .contains(&needle)
        })
        .collect();
    found.sort_by(|a, b| (a.category, a.name).cmp(&(b.category, b.name)));
    found
}

fn category_counts_json() -> Json {
    Json::obj(
        registry::category_counts()
            .into_iter()
            .map(|(category, n)| (category, Json::Int(n as i64))),
    )
}

/// The body of `list_indicators(query, category)`: the matches with a
/// description each, or their names by category when there are more than
/// 45.
pub(crate) fn list_indicators(query: &str, category: &str) -> Result<Json, ToolOutcome> {
    let wanted = category.trim().to_lowercase();
    if !wanted.is_empty() && !CATEGORIES.contains(&wanted.as_str()) {
        return Err(refuse(
            "category",
            &format!(
                "{} is not one of the catalogue's categories",
                py_repr(&Value::String(wanted))
            ),
            &format!("Use one of: {}.", CATEGORIES.join(", ")),
        ));
    }
    let specs = search_specs(query, &wanted);
    if specs.is_empty() {
        return Err(refuse(
            "query",
            &format!(
                "nothing in the catalogue matches {}",
                py_repr(&Value::String(query.into()))
            ),
            "Try a broader keyword, or call list_indicators with no arguments to see every \
             category.",
        ));
    }
    if specs.len() > LIST_DETAIL_LIMIT {
        let mut grouped = Json::Obj(Vec::new());
        for spec in &specs {
            let mut names = match grouped.get(spec.category) {
                Some(Json::Arr(items)) => items.clone(),
                _ => Vec::new(),
            };
            names.push(Json::str(spec.name));
            grouped.set(spec.category, Json::Arr(names));
        }
        return Ok(Json::obj([
            ("total", Json::Int(specs.len() as i64)),
            ("detail_omitted", Json::Bool(true)),
            ("by_category", grouped),
            ("category_counts", category_counts_json()),
            (
                "note",
                Json::str(
                    "Names only, because the whole catalogue with a description each does not \
                     fit one result. Pass a query such as 'bollinger' or a category to get \
                     descriptions, or call describe_indicator for one name.",
                ),
            ),
        ]));
    }
    Ok(Json::obj([
        ("total", Json::Int(specs.len() as i64)),
        ("categories", strings(&CATEGORIES)),
        (
            "indicators",
            Json::Arr(specs.iter().map(|s| spec_to_dict(s, false)).collect()),
        ),
    ]))
}

/// The body of `describe_indicator(name)`: the full signature.
pub(crate) fn describe_indicator(name: &str) -> Result<Json, ToolOutcome> {
    let cleaned = name.trim().to_lowercase();
    match registry::get_spec(&cleaned) {
        Some(spec) => Ok(spec_to_dict(spec, true)),
        None => Err(unknown_indicator("name", name)),
    }
}

/// `_unknown_indicator`: the refusal for a name that is not registered,
/// naming up to five near matches when the catalogue has any.
pub(crate) fn unknown_indicator(field: &str, name: &str) -> ToolOutcome {
    let cleaned = name.trim().to_lowercase();
    let near = if cleaned.is_empty() {
        Vec::new()
    } else {
        search_specs(&cleaned, "")
    };
    let shown = py_repr(&Value::String(name.into()));
    if near.is_empty() {
        return refuse(
            field,
            &format!(
                "{} is not one of the {} indicators",
                shown,
                registry::count()
            ),
            "Call list_indicators with a keyword to find the right name.",
        );
    }
    let names: Vec<&str> = near.iter().take(5).map(|s| s.name).collect();
    refuse(
        field,
        &format!("{} is not an indicator on this build", shown),
        &format!("Did you mean: {}?", names.join(", ")),
    )
}
