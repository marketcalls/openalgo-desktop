//! The catalogue against the web: the names the node runs, which nest, the
//! refusal sets, the required defaults, what a float does to each integer
//! parameter (`params.json`), and the editor's own lists
//! (`src/lib/flow/constants.ts`, carried from the web).

use regex::Regex;
use serde_json::{json as value, Value};

use super::support::{json, Failures, CATALOGUE, PARAMS};
use crate::agent::indicators::{get_spec, required_default, Kind, ParamDefault};
use crate::flow::indicator_service::params::bind;
use crate::flow::indicator_service::{
    is_single_series_indicator, list_supported_indicators, IndicatorParams, TWO_SERIES_INDICATORS,
    UNSUPPORTED_INDICATORS,
};

const EDITOR: &str = include_str!("../../../../../src/lib/flow/constants.ts");

#[test]
fn the_supported_names_are_the_webs() {
    let fixture = json(CATALOGUE);
    assert_eq!(value!(list_supported_indicators()), fixture["supported"]);
    let mut two: Vec<&str> = TWO_SERIES_INDICATORS.to_vec();
    two.sort_unstable();
    assert_eq!(value!(two), fixture["two_series"]);
    let mut unsupported: Vec<&str> = UNSUPPORTED_INDICATORS.to_vec();
    unsupported.sort_unstable();
    assert_eq!(value!(unsupported), fixture["unsupported"]);
}

#[test]
fn nesting_follows_the_signature() {
    let fixture = json(CATALOGUE);
    let probes = fixture["single_series"].as_object().expect("probes");
    let mut failures = Failures::default();
    for (name, want) in probes {
        if value!(is_single_series_indicator(name)) != *want {
            failures.push(name, format!("web says {want}"));
        }
    }
    failures.check(probes.len());
}

#[test]
fn required_defaults_are_the_webs() {
    let fixture = json(CATALOGUE);
    let table = fixture["required_defaults"].as_object().expect("table");
    for (name, params) in table {
        let (key, default) = params
            .as_object()
            .and_then(|p| p.iter().next())
            .expect("one");
        assert_eq!(
            required_default(name),
            Some((key.as_str(), default.as_i64().expect("int")))
        );
    }
}

/// The integer parameters the web's method uses as a number.
const USED_AS_NUMBER: [(&str, &str); 4] = [
    ("hv", "annual"),
    ("hv", "per"),
    ("lrslope", "interval"),
    ("pvi_with_signal", "signal_length"),
];

/// What a fractional float does to each integer parameter is the web's:
/// computed, or refused with the library's `TypeError` (the
/// `validate_period` arguments and the four that reach `range()` or a
/// slice).
#[test]
fn a_float_for_an_integer_is_refused_where_the_web_refuses_it() {
    let fixture = json(PARAMS);
    let cases = fixture["float_for_int"].as_array().expect("cases");
    let mut failures = Failures::default();
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let param = case["param"].as_str().expect("param");
        let spec = get_spec(name).expect("spec");
        let p = spec.param(param).expect("param");
        let got = bind(
            spec,
            &IndicatorParams::from_pairs([(
                param.to_string(),
                value!(match p.default {
                    ParamDefault::Int(v) => v as f64 + 0.5,
                    _ => 14.5,
                }),
            )]),
        );
        let refused = got
            .err()
            .map(|e| value!({"kind": e.kind, "message": e.message}));
        // Binding decides the type refusals; a library error comes later.
        // The four parameters the web's method uses as a number compute
        // there with the fraction; the desktop refuses it (a recorded
        // deviation, `params::FRACTION_REFUSED`).
        let want = match case["refusal"]["kind"].as_str() {
            Some("TypeError") => case["refusal"].clone(),
            _ if USED_AS_NUMBER.contains(&(name, param)) => value!({
                "kind": "TypeError",
                "message": format!("{name}: {param} must be a whole number, got {}", match p.default {
                    ParamDefault::Int(v) => format!("{}", v as f64 + 0.5),
                    _ => "14.5".into(),
                }),
            }),
            _ => Value::Null,
        };
        if refused.as_ref().unwrap_or(&Value::Null) != &want {
            failures.push(
                format!("{name}.{param}"),
                format!("{refused:?} vs {}", case["refusal"]),
            );
        }
    }
    failures.check(cases.len());
}

/// The block of `constants.ts` that starts at `start` and ends at the first
/// line that closes it.
fn block<'a>(start: &str, end: &str) -> &'a str {
    let from = EDITOR.find(start).expect("block start");
    let rest = &EDITOR[from..];
    &rest[..rest.find(end).expect("block end")]
}

/// The editor offers exactly the names the node runs (IND-092).
#[test]
fn the_editor_lists_the_supported_names() {
    let catalog = block("export const INDICATOR_CATALOG", "] as const");
    let values = Regex::new(r"\{ value: '([a-z0-9_]+)'").expect("regex");
    let mut offered: Vec<&str> = values
        .captures_iter(catalog)
        .map(|c| c.get(1).map_or("", |m| m.as_str()))
        .collect();
    offered.sort_unstable();
    assert_eq!(offered, list_supported_indicators());
}

/// Every parameter field the editor renders is the library's parameter,
/// with its type and default (required ones at `REQUIRED_PARAM_DEFAULTS`).
#[test]
fn the_editors_parameter_fields_are_the_librarys() {
    let params = block("export const INDICATOR_PARAMS", "\n}\n");
    let key = Regex::new(r"(?m)^  ([a-z0-9_]+): \[").expect("regex");
    let field =
        Regex::new(r"(?s)\{\s*name: '([a-z0-9_]+)',.*?type: '(\w+)',\s*default: ([^,}\n]+)")
            .expect("regex");
    let starts: Vec<(usize, &str)> = key
        .captures_iter(params)
        .filter_map(|c| Some((c.get(0)?.start(), c.get(1)?.as_str())))
        .collect();
    let mut failures = Failures::default();
    for (i, (at, name)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(params.len(), |(next, _)| *next);
        let spec = get_spec(name).expect("a library method");
        let fields: Vec<(String, String, String)> = field
            .captures_iter(&params[*at..end])
            .map(|c| (c[1].to_string(), c[2].to_string(), c[3].trim().to_string()))
            .collect();
        let library: Vec<(String, String, String)> = spec
            .params
            .iter()
            .map(|p| {
                let kind = match p.kind {
                    Kind::Int => "int",
                    Kind::Float => "float",
                    Kind::Str => "string",
                    Kind::Bool => "bool",
                };
                let default = match p.default {
                    ParamDefault::Required => required_default(name)
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default(),
                    ParamDefault::Int(v) => v.to_string(),
                    ParamDefault::Float(v) => format!("{v:?}"),
                    ParamDefault::Str(v) => format!("'{v}'"),
                    ParamDefault::Bool(v) => v.to_string(),
                };
                (p.name.to_string(), kind.to_string(), default)
            })
            .collect();
        if fields != library {
            failures.push(name, format!("editor {fields:?} vs library {library:?}"));
        }
    }
    assert_eq!(starts.len(), list_supported_indicators().len());
    failures.check(starts.len());
}
