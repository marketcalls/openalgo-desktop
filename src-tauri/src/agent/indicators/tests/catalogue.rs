//! The catalogue against the web's registry as it renders on openalgo 2.0.5
//! (`registry.json`, with the desktop's `vi` and `ulcerindex` enabled), and
//! the discovery tools' bodies and refusals.

use serde_json::Value;

use super::support::{json, json_diff, COMPUTE, REGISTRY};
use crate::agent::indicators::descriptions::describe;
use crate::agent::indicators::discovery::{
    describe_indicator, list_indicators, search_specs, spec_to_dict, unknown_indicator,
};
use crate::agent::indicators::registry::{
    self, category_counts, get_spec, required_default, Default, CATEGORIES,
};

fn strings(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.to_string())).collect())
}

fn default_value(default: Default) -> Value {
    match default {
        Default::Required => Value::Null,
        Default::Int(v) => v.into(),
        Default::Float(v) => v.into(),
        Default::Str(s) => s.into(),
        Default::Bool(b) => b.into(),
    }
}

#[test]
fn every_entry_matches_the_web_registry() {
    let fixture = json(REGISTRY);
    let specs = fixture["specs"].as_array().expect("specs");
    assert_eq!(registry::count(), 127);
    assert_eq!(specs.len(), 127);
    for (spec, want) in registry::all().zip(specs) {
        let name = spec.name;
        assert_eq!(want["name"], name);
        assert_eq!(want["category"], spec.category, "{name}");
        assert_eq!(want["inputs"], strings(spec.inputs), "{name}");
        assert_eq!(want["outputs"], strings(spec.outputs), "{name}");
        assert_eq!(want["status"], spec.status.as_str(), "{name}");
        assert_eq!(
            want["alias_of"],
            spec.alias_of.map_or(Value::Null, Value::from),
            "{name}"
        );
        assert_eq!(want["warmup"], spec.warmup as u64, "{name}");
        assert_eq!(want["note"], spec.note, "{name}");
        assert_eq!(want["series_args"], strings(spec.series_args), "{name}");
        assert_eq!(
            want["needs_second_series"], spec.needs_second_series,
            "{name}"
        );
        assert_eq!(want["description"], describe(name), "{name}");
        let conditional: serde_json::Map<String, Value> = spec
            .conditional_outputs
            .iter()
            .map(|(k, v)| (k.to_string(), strings(v)))
            .collect();
        let want_conditional = want["conditional_outputs"].clone();
        if conditional.is_empty() {
            assert_eq!(want_conditional, Value::Null, "{name}");
        } else {
            assert_eq!(want_conditional, Value::Object(conditional), "{name}");
        }
        let params: Vec<Value> = spec
            .params
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "kind": p.kind.as_str(),
                    "default": default_value(p.default),
                    "enum": if p.allowed.is_empty() { Value::Null } else { strings(p.allowed) },
                    "required": p.required(),
                })
            })
            .collect();
        assert_eq!(want["params"], Value::Array(params), "{name}");
    }
}

#[test]
fn the_desktop_enables_vi_and_ulcerindex_and_records_why() {
    let fixture = json(REGISTRY);
    for name in ["vi", "ulcerindex"] {
        assert_eq!(fixture["web_overrides"][name]["status"], "broken");
        let spec = get_spec(name).expect("registered");
        assert_eq!(spec.status.as_str(), "ok");
        assert!(spec.warmup > 0, "{name} has a measured warm-up");
    }
}

#[test]
fn categories_counts_and_required_defaults_match() {
    let fixture = json(REGISTRY);
    assert_eq!(fixture["categories"], strings(&CATEGORIES));
    for (category, n) in category_counts() {
        assert_eq!(fixture["category_counts"][category], n as u64, "{category}");
    }
    let wanted = fixture["required_param_defaults"]
        .as_object()
        .expect("defaults");
    for spec in registry::all() {
        match (required_default(spec.name), wanted.get(spec.name)) {
            (Some((key, value)), Some(want)) => {
                assert_eq!(want, &serde_json::json!({ key: value }), "{}", spec.name)
            }
            (None, None) => {}
            other => panic!("{}: {other:?}", spec.name),
        }
    }
}

#[test]
fn rendered_specs_match_the_web() {
    let fixture = json(REGISTRY);
    for want in fixture["specs"].as_array().expect("specs") {
        let spec = get_spec(want["name"].as_str().expect("name")).expect("spec");
        for (verbose, key) in [(false, "brief"), (true, "verbose")] {
            let got = serde_json::to_value(spec_to_dict(spec, verbose)).expect("serialises");
            assert_eq!(
                json_diff(key, &got, &want[key], 0.0, 0.0),
                None,
                "{}",
                spec.name
            );
        }
    }
    let described = describe_indicator(" MACD ").expect("known");
    assert_eq!(
        serde_json::to_value(described).expect("serialises"),
        fixture["specs"]
            .as_array()
            .and_then(|s| s.iter().find(|x| x["name"] == "macd"))
            .map(|x| x["verbose"].clone())
            .expect("macd")
    );
}

#[test]
fn search_finds_what_the_web_finds() {
    let fixture = json(COMPUTE);
    for case in fixture["searches"].as_array().expect("searches") {
        let query = case["query"].as_str().unwrap_or_default();
        let category = case["category"].as_str().unwrap_or_default();
        let names: Vec<&str> = search_specs(query, category)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(strings(&names), case["names"], "{query:?} {category:?}");
    }
}

#[test]
fn list_indicators_details_a_narrow_match_and_groups_a_broad_one() {
    let narrow =
        serde_json::to_value(list_indicators("bollinger", "").expect("matches")).expect("json");
    let names: Vec<&str> = search_specs("bollinger", "")
        .iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(narrow["total"], names.len() as u64);
    assert_eq!(narrow["categories"], strings(&CATEGORIES));
    for (entry, name) in narrow["indicators"]
        .as_array()
        .expect("indicators")
        .iter()
        .zip(&names)
    {
        let spec = get_spec(name).expect("spec");
        assert_eq!(
            entry,
            &serde_json::to_value(spec_to_dict(spec, false)).expect("json")
        );
    }

    let broad = list_indicators("", "").expect("everything");
    assert_eq!(
        broad.get("total"),
        Some(&crate::agent::indicators::json::Json::Int(127))
    );
    let broad = serde_json::to_value(broad).expect("json");
    assert_eq!(broad["detail_omitted"], true);
    assert_eq!(
        broad["by_category"]["hybrid"],
        strings(&[
            "adx",
            "aroon",
            "dmi",
            "fractals",
            "pivot_points",
            "psar",
            "rwi"
        ])
    );
    assert_eq!(broad["category_counts"]["volume"], 17);
    assert!(broad["note"]
        .as_str()
        .expect("note")
        .starts_with("Names only, because"));
}

#[test]
fn list_and_describe_refusals_are_the_webs_sentences() {
    let bad_category = list_indicators("", "Trends").expect_err("refused");
    assert_eq!(
        bad_category.text(),
        "The 'category' argument is invalid: 'trends' is not one of the catalogue's categories \
         Use one of: trend, momentum, volatility, volume, oscillators, statistical, hybrid, \
         talib_extra, utility."
    );
    let nothing = list_indicators("zzz", "").expect_err("refused");
    assert_eq!(
        nothing.text(),
        "The 'query' argument is invalid: nothing in the catalogue matches 'zzz' Try a broader \
         keyword, or call list_indicators with no arguments to see every category."
    );
    let near = describe_indicator("bollinger").expect_err("unknown");
    assert_eq!(
        near.text(),
        "The 'name' argument is invalid: 'bollinger' is not an indicator on this build Did you \
         mean: bbands, bbpercent, bbwidth, obv_smoothed?"
    );
    let far = unknown_indicator("indicator", "qqq");
    assert_eq!(
        far.text(),
        "The 'indicator' argument is invalid: 'qqq' is not one of the 127 indicators Call \
         list_indicators with a keyword to find the right name."
    );
}

/// The web asserts its registry against `dir(ta)` at import; here every
/// entry must reach a `ta` method, once, and every alias must name one.
#[test]
fn the_catalogue_and_the_ta_surface_agree() {
    use crate::agent::indicators::ta::{self, Params};
    let mut seen = std::collections::BTreeSet::new();
    for spec in registry::all() {
        assert!(seen.insert(spec.name), "{} is registered twice", spec.name);
        let empty = Params::new(spec.name, Vec::new());
        assert!(
            ta::call(spec.name, &[], &empty).is_some(),
            "ta.{} is not dispatched",
            spec.name
        );
        if let Some(target) = spec.alias_of {
            assert!(
                get_spec(target).is_some(),
                "{} aliases unknown {target}",
                spec.name
            );
        }
    }
    let empty = Params::new("nope", Vec::new());
    assert!(ta::call("not_an_indicator", &[], &empty).is_none());
    let sma = get_spec("sma").expect("sma");
    let missing = ta::call(
        "sma",
        &[],
        &crate::agent::indicators::compute::bind(sma, &[]),
    );
    assert_eq!(
        missing.and_then(Result::err).map(|e| (e.kind, e.message)),
        Some((
            "TypeError",
            "TechnicalAnalysis.sma() missing 1 required positional argument: 'data'".to_string()
        ))
    );
}

/// The web's TECHNICAL INDICATORS prompt section tells the model to compute
/// these names straight away (`services/agent/prompts.py`); each must exist.
#[test]
fn the_names_the_prompt_promises_are_registered() {
    let promised = [
        "rsi",
        "macd",
        "supertrend",
        "atr",
        "adx",
        "bbands",
        "sma",
        "ema",
        "vwap",
        "stochastic",
        "obv",
        "cci",
        "mfi",
        "psar",
        "ichimoku",
        "keltner",
        "donchian",
        "cmf",
    ];
    for name in promised {
        assert!(get_spec(name).is_some(), "{name}");
    }
}
