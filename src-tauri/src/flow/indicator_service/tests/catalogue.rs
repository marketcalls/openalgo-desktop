//! The catalogue against the web: the names the node runs, which nest, the
//! refusal sets, the required defaults, what a float does to each integer
//! parameter (`params.json`), and the editor's own lists
//! (`src/lib/flow/constants.ts`, carried from the web) against the library
//! and against the web editor's (`editor.json`).

use regex::Regex;
use serde_json::{json as value, Value};

use super::support::{json, Failures, CATALOGUE, EDITOR_LISTS, PARAMS};
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

/// What a fractional float does to each integer parameter is the web's:
/// computed (with the fraction where the method uses the number), or
/// refused with the library's `TypeError`.
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
            150,
        );
        let refused = got
            .err()
            .map(|e| value!({"kind": e.kind, "message": e.message}));
        // Binding decides the type refusals; a library error comes later.
        let want = match case["refusal"]["kind"].as_str() {
            Some("TypeError") => case["refusal"].clone(),
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

/// The block of `text` that starts at `start` and ends at the first `end`.
fn block<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).expect("block start");
    let rest = &text[from..];
    &rest[..rest.find(end).expect("block end")]
}

/// The editor's `INDICATOR_CATALOG` values, sorted. Line endings are
/// normalised first: a Windows checkout writes the file with CRLF.
fn catalog_values(editor: &str) -> Vec<String> {
    let editor = editor.replace("\r\n", "\n");
    let catalog = block(&editor, "export const INDICATOR_CATALOG", "] as const");
    let values = Regex::new(r"\{ value: '([a-z0-9_]+)'").expect("regex");
    let mut offered: Vec<String> = values
        .captures_iter(catalog)
        .map(|c| c[1].to_string())
        .collect();
    offered.sort_unstable();
    offered
}

/// One editor field: name, type and default as written.
type Field = (String, String, String);

/// The editor's `INDICATOR_PARAMS`: each indicator's fields, in order.
fn editor_params(editor: &str) -> Vec<(String, Vec<Field>)> {
    let editor = editor.replace("\r\n", "\n");
    let params = block(&editor, "export const INDICATOR_PARAMS", "\n}\n");
    let key = Regex::new(r"(?m)^  ([a-z0-9_]+): \[").expect("regex");
    let field =
        Regex::new(r"(?s)\{\s*name: '([a-z0-9_]+)',.*?type: '(\w+)',\s*default: ([^,}\n]+)")
            .expect("regex");
    let starts: Vec<(usize, &str)> = key
        .captures_iter(params)
        .filter_map(|c| Some((c.get(0)?.start(), c.get(1)?.as_str())))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, (at, name))| {
            let end = starts.get(i + 1).map_or(params.len(), |(next, _)| *next);
            let fields: Vec<Field> = field
                .captures_iter(&params[*at..end])
                .map(|c| (c[1].to_string(), c[2].to_string(), c[3].trim().to_string()))
                .collect();
            (name.to_string(), fields)
        })
        .collect()
}

/// The editor offers exactly the names the node runs (IND-092).
#[test]
fn the_editor_lists_the_supported_names() {
    assert_eq!(catalog_values(EDITOR), list_supported_indicators());
}

/// A Windows checkout (CRLF line endings) reads the same as LF.
#[test]
fn the_editor_file_reads_the_same_with_crlf_line_endings() {
    let crlf = EDITOR.replace("\r\n", "\n").replace('\n', "\r\n");
    assert!(crlf.contains("\r\n"));
    assert_eq!(catalog_values(&crlf), catalog_values(EDITOR));
    assert_eq!(editor_params(&crlf), editor_params(EDITOR));
    assert_eq!(
        editor_params(&crlf).len(),
        list_supported_indicators().len()
    );
}

/// Every parameter field the editor renders is the library's parameter,
/// with its type and default (required ones at `REQUIRED_PARAM_DEFAULTS`).
#[test]
fn the_editors_parameter_fields_are_the_librarys() {
    let editor = editor_params(EDITOR);
    let mut failures = Failures::default();
    for (name, fields) in &editor {
        let spec = get_spec(name).expect("a library method");
        let library: Vec<Field> = spec
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
        if *fields != library {
            failures.push(name, format!("editor {fields:?} vs library {library:?}"));
        }
    }
    assert_eq!(editor.len(), list_supported_indicators().len());
    failures.check(editor.len());
}

/// The editor's categories and catalogue entries are the web editor's, label
/// and category included, and every entry's category is one of the
/// categories (F-250).
#[test]
fn the_editors_catalogue_is_the_webs() {
    let web = json(EDITOR_LISTS);
    let editor = EDITOR.replace("\r\n", "\n");
    let categories_block = block(&editor, "export const INDICATOR_CATEGORIES", "] as const");
    let category = Regex::new(r"(?m)^  '([^']+)',$").expect("regex");
    let categories: Vec<String> = category
        .captures_iter(categories_block)
        .map(|c| c[1].to_string())
        .collect();
    assert_eq!(value!(categories), web["categories"]);
    let entry = Regex::new(r"\{ value: '([^']+)', label: '([^']+)', category: '([^']+)' \}")
        .expect("regex");
    let catalog: Vec<Value> = entry
        .captures_iter(block(
            &editor,
            "export const INDICATOR_CATALOG",
            "] as const",
        ))
        .map(|c| value!({"value": &c[1], "label": &c[2], "category": &c[3]}))
        .collect();
    assert_eq!(value!(catalog), web["catalog"]);
    for item in &catalog {
        assert!(categories.iter().any(|c| *c == item["category"]), "{item}");
    }
}

/// Every parameter field's label and choices are the web editor's, and
/// where the library names the values it accepts, the choices are exactly
/// those (F-251; names, types and defaults are pinned against the library
/// by `the_editors_parameter_fields_are_the_librarys`).
#[test]
fn the_editors_parameter_labels_and_choices_are_the_webs() {
    let web = json(EDITOR_LISTS);
    let editor = EDITOR.replace("\r\n", "\n");
    let params = block(&editor, "export const INDICATOR_PARAMS", "\n}\n");
    let key = Regex::new(r"(?m)^  ([a-z0-9_]+): \[").expect("regex");
    let field = Regex::new(
        r"(?s)\{\s*name: '([^']+)',\s*label: '([^']+)',\s*type: '(\w+)',\s*default: ([^,}\n]+?),?\s*(?:choices: \[([^\]]*)\],?\s*)?\}",
    )
    .expect("regex");
    let quoted = Regex::new(r"'([^']*)'").expect("regex");
    let starts: Vec<(usize, String)> = key
        .captures_iter(params)
        .filter_map(|c| Some((c.get(0)?.start(), c[1].to_string())))
        .collect();
    let mut failures = Failures::default();
    for (i, (at, name)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(params.len(), |(next, _)| *next);
        let fields: Vec<Value> = field
            .captures_iter(&params[*at..end])
            .map(|c| {
                let choices = c.get(5).map(|m| {
                    quoted
                        .captures_iter(m.as_str())
                        .map(|q| q[1].to_string())
                        .collect::<Vec<_>>()
                });
                value!({"name": &c[1], "label": &c[2], "type": &c[3],
                        "default": c[4].trim(), "choices": choices})
            })
            .collect();
        if value!(fields) != web["params"][name] {
            failures.push(name, format!("{fields:?} vs {}", web["params"][name]));
        }
        let spec = get_spec(name).expect("a library method");
        for f in &fields {
            let allowed = spec
                .param(f["name"].as_str().unwrap_or_default())
                .map(|p| p.allowed);
            if let (Some(allowed), Some(choices)) = (allowed, f["choices"].as_array()) {
                if !allowed.is_empty() && value!(allowed) != value!(choices) {
                    failures.push(name, format!("choices {choices:?} vs library {allowed:?}"));
                }
            }
        }
    }
    let total = web["params"].as_object().map_or(0, |m| m.len());
    assert_eq!(starts.len(), total);
    failures.check(starts.len());
}
