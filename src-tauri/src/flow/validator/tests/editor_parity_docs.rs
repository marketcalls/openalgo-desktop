//! Parity X-263: the web's `test_prompt_prose_documents_every_repaired_flow_contract`
//! (`test/test_flow_workflow_validator.py` 245-441) over the desktop's
//! `docs/prompt/flow-import-format.md`: the prose keeps every correction
//! behind the examples.
//!
//! Written by the docs lane on pr/flow-docs, which has no validator tests
//! module. At restack, declare it in `tests/mod.rs` (`mod editor_parity_docs;`)
//! and drop the `#[ignore]`. Until then nothing declares it, so it is
//! marked test-only on its own.
#![cfg(test)]

use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The import-format document, line endings as LF (a Windows checkout may
/// hold it with CRLF).
fn import_format() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/prompt/flow-import-format.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// One Markdown section by its heading prefix, subsections included, up to
/// its next peer (the web's `_markdown_section`).
fn section<'a>(doc: &'a str, prefix: &str) -> &'a str {
    let head = Regex::new(&format!(r"(?m)^{}(?:\s.*)?$", regex::escape(prefix))).unwrap();
    let found = head
        .find(doc)
        .unwrap_or_else(|| panic!("missing prompt heading: {prefix}"));
    let level = prefix.len() - prefix.trim_start_matches('#').len();
    let next = Regex::new(&format!(r"(?m)^#{{1,{level}}}\s")).unwrap();
    let end = next
        .find(&doc[found.end()..])
        .map_or(doc.len(), |n| found.end() + n.start());
    &doc[found.start()..end]
}

/// Every node (`type` a string, `data` an object) anywhere inside every
/// value of every json fence (the web's `_prompt_json_nodes`).
fn documented_nodes(doc: &str) -> Vec<Value> {
    fn walk(v: &Value, out: &mut Vec<Value>) {
        if v.get("type").is_some_and(Value::is_string)
            && v.get("data").is_some_and(Value::is_object)
        {
            out.push(v.clone());
        }
        match v {
            Value::Object(m) => m.values().for_each(|c| walk(c, out)),
            Value::Array(a) => a.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let fence = Regex::new(r"(?ms)^```json\s*\n(.*?)^```\s*$").unwrap();
    let mut out = Vec::new();
    for caps in fence.captures_iter(doc) {
        for value in serde_json::Deserializer::from_str(&caps[1]).into_iter::<Value>() {
            walk(&value.unwrap(), &mut out);
        }
    }
    out
}

/// Whether any object at or under `v` has `key` (the web's walk over a
/// node's legs).
fn holds_key(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(m) => m.contains_key(key) || m.values().any(|c| holds_key(c, key)),
        Value::Array(a) => a.iter().any(|c| holds_key(c, key)),
        _ => false,
    }
}

#[test]
#[ignore = "needs pr/flow-validator at restack"]
fn the_import_format_prose_documents_every_repaired_contract() {
    let doc = import_format();
    let mut failures: Vec<String> = Vec::new();
    let mut require = |what: &str, ok: bool| {
        if !ok {
            failures.push(what.to_string());
        }
    };

    let trigger = section(&doc, "### 7.1 Trigger nodes");
    require(
        "second_trigger",
        trigger.contains("reject a second trigger") && !trigger.contains("permit a second trigger"),
    );

    let history = section(&doc, "#### history");
    require(
        "history_range",
        ["`days`", "`startDate`", "`endDate`"]
            .iter()
            .all(|f| history.contains(f))
            && history.contains("explicit range takes\nprecedence over `days`"),
    );

    let holidays = section(&doc, "#### holidays");
    require(
        "holidays_year",
        holidays.contains("`year`") && !holidays.contains("`exchange`"),
    );
    let timings = section(&doc, "#### timings");
    require(
        "timings_date",
        timings.contains("`date`") && !timings.contains("`exchange`"),
    );

    let variable = section(&doc, "#### variable");
    let after_head = variable
        .split_once("| Operation | Behaviour |")
        .map_or(variable, |(_, rest)| rest);
    let table = after_head
        .split_once("| Field | Type | Default | Notes |")
        .map_or(after_head, |(head, _)| head);
    let operation = Regex::new(r#"(?m)^\| `"([^"]+)"` \|"#).unwrap();
    let documented: BTreeSet<&str> = operation
        .captures_iter(table)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let expected: BTreeSet<&str> = [
        "set",
        "get",
        "add",
        "subtract",
        "multiply",
        "divide",
        "increment",
        "decrement",
        "parse_json",
        "stringify",
        "append",
    ]
    .into_iter()
    .collect();
    require("variable_operations", documented == expected);
    require(
        "variable_conditional_fields",
        variable.contains("Required for `add`, `subtract`, `multiply`, and `divide`")
            && variable.contains("`parse_json` requires a non-empty value")
            && variable.contains("Required for `get` and `stringify`"),
    );

    let math = section(&doc, "#### mathExpression");
    require(
        "floor_only",
        math.contains("sole allowed function `floor(expression)`")
            && math.contains("Other calls")
            && math.contains("rejected"),
    );

    let telegram = section(&doc, "#### telegramAlert");
    require(
        "telegram_identity",
        telegram.contains("workflow API key")
            && telegram.contains("API-key owner")
            && telegram.contains("cannot supply a\nrecipient override")
            && !telegram.to_lowercase().contains("username"),
    );

    require(
        "http_timeout_range",
        section(&doc, "#### httpRequest").contains("between 1000 and 60000"),
    );
    require(
        "http_timeout_example",
        section(&doc, "### 8.7 Webhook").contains("\"timeout\": 10000"),
    );

    for node_type in ["smartOrder", "basketOrder", "splitOrder"] {
        let order = section(&doc, &format!("#### {node_type}"));
        let what = format!("{node_type}_common_prices");
        require(
            what.as_str(),
            order.contains("`price`")
                && order.contains("`triggerPrice`")
                && order.contains("positive for `LIMIT`/`SL`")
                && order.contains("positive for `SL`/`SL-M`"),
        );
    }

    let multi = section(&doc, "#### optionsMultiOrder");
    require(
        "options_multi_leg_expiry",
        multi.contains("`expiry`")
            && multi.contains("DDMMMYY")
            && multi.contains("`expiryType`")
            && multi.to_lowercase().contains("calendar"),
    );
    require(
        "options_multi_leg_strike",
        multi.contains("`strikeMode`")
            && multi.contains("`OFFSET`")
            && multi.contains("`STRIKE`")
            && multi.contains("Required unless `strike` is given"),
    );
    require(
        "options_multi_price_types",
        multi.contains(r#"`priceType` | `"MARKET"` \| `"LIMIT"`"#)
            && multi.contains("generated legs do not support `SL`/`SL-M`")
            && multi.contains("a custom leg may use `SL`/`SL-M`"),
    );

    let nodes = documented_nodes(&doc);
    require(
        "telegram_json_username",
        !nodes
            .iter()
            .any(|n| n["type"] == "telegramAlert" && n["data"].get("username").is_some()),
    );
    require(
        "options_multi_json_leg_expiry",
        !nodes.iter().any(|n| {
            n["type"] == "optionsMultiOrder"
                && ["legs", "orderLegs"].iter().any(|k| {
                    n["data"]
                        .get(*k)
                        .is_some_and(|legs| holds_key(legs, "expiryDate"))
                })
        }),
    );

    let pnl = section(&doc, "### 8.5 P&L stop-loss");
    require(
        "computed_pnl_condition",
        pnl.contains("\"type\": \"varCondition\"") && !pnl.contains("\"type\": \"priceCondition\""),
    );

    require(
        "place_order_index_exchange",
        !nodes.iter().any(|n| {
            n["type"] == "placeOrder"
                && matches!(
                    n["data"]["exchange"].as_str(),
                    Some("NSE_INDEX" | "BSE_INDEX")
                )
        }),
    );

    assert!(
        failures.is_empty(),
        "prompt prose contract failures: {failures:?}"
    );
}
