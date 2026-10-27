//! The indicator tools' own argument handling (the web's
//! `IndicatorsToolkit._tail`, `_lookback`, `_params`, `_decoded_list`,
//! `_batch_requests` and `_scan_symbols`): numbers the model sends as text,
//! objects it sends as JSON text, lists it sends as comma text, and the caps
//! that bound one call.

use crate::agent::contract::tool::ToolOutcome;
use crate::agent::indicators::{get_spec, unknown_indicator};
use crate::agent::tools::market::normalise::refuse;
use crate::agent::tools::values::{py_str, quote, truthy, type_name};
use serde_json::{Map, Value};

/// Most indicators one batch call may carry (`MAX_BATCH_INDICATORS`).
pub(super) const MAX_BATCH_INDICATORS: usize = 8;

/// Most instruments one scan may cover (`MAX_SCAN_SYMBOLS`).
pub(super) const MAX_SCAN_SYMBOLS: usize = 15;

/// Most candles one call computes over when the model names the lookback
/// (`MAX_LOOKBACK_BARS`).
pub(super) const MAX_LOOKBACK_BARS: i64 = 5000;

/// Most values one output returns (`MAX_LAST_N`).
pub(super) const MAX_LAST_N: i64 = 120;

/// One batch entry: the indicator name and its parameters.
pub(super) type Request = (String, Map<String, Value>);

/// Python's `int(value)` for a JSON value: a whole number from an int, a
/// float (truncated), a bool or numeric text; `None` where Python raises.
fn py_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            t.parse::<i64>().ok()
        }
        _ => None,
    }
}

/// `_tail`: values per output, 1..=120; anything unusable means `fallback`.
pub(super) fn tail(last_n: Option<&Value>, fallback: i64) -> Result<i64, ToolOutcome> {
    let Some(wanted) = py_int(last_n) else {
        return Ok(fallback);
    };
    if wanted < 1 {
        return Ok(fallback);
    }
    if wanted > MAX_LAST_N {
        return Err(refuse(
            "last_n",
            &format!(
                "{} values per output is more than the {} one result carries",
                wanted, MAX_LAST_N
            ),
            &format!(
                "Ask for at most {}, or narrow the question. Every value costs context, and a \
                 table longer than this is not read.",
                MAX_LAST_N
            ),
        ));
    }
    Ok(wanted)
}

/// `_lookback`: candles to compute over, or 0 to size from the warm-up.
pub(super) fn lookback(lookback_bars: Option<&Value>) -> Result<usize, ToolOutcome> {
    let Some(wanted) = py_int(lookback_bars) else {
        return Ok(0);
    };
    if wanted <= 0 {
        return Ok(0);
    }
    if wanted > MAX_LOOKBACK_BARS {
        return Err(refuse(
            "lookback_bars",
            &format!(
                "{} candles is more than the {} one call computes over",
                wanted, MAX_LOOKBACK_BARS
            ),
            &format!(
                "Ask for at most {}, or leave it 0 and let the indicator's own warm-up size the \
                 window, which is what you want unless you are deliberately looking further back.",
                MAX_LOOKBACK_BARS
            ),
        ));
    }
    Ok(wanted as usize)
}

/// `_params`: an object, or an object sent as JSON text; nothing is `{}`.
pub(super) fn params(
    value: Option<&Value>,
    field: &str,
) -> Result<Map<String, Value>, ToolOutcome> {
    match value {
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(Value::String(text)) if !text.trim().is_empty() => {
            match serde_json::from_str::<Value>(text) {
                Ok(Value::Object(map)) => Ok(map),
                _ => {
                    let shown: String = text.trim().chars().take(60).collect();
                    Err(refuse(
                        field,
                        &format!("{} is not a set of named parameters", quote(&shown)),
                        "Pass an object, for example {\"period\": 21}.",
                    ))
                }
            }
        }
        _ => Ok(Map::new()),
    }
}

/// `_decoded_list`: a JSON array or object sent as text is decoded, other
/// text is split on commas; anything not text is returned as it is.
pub(super) fn decoded_list(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let Value::String(text) = value else {
        return Some(value.clone());
    };
    let trimmed = text.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if let Ok(decoded) = serde_json::from_str::<Value>(trimmed) {
            return Some(decoded);
        }
    }
    Some(Value::Array(
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| Value::String(part.to_string()))
            .collect(),
    ))
}

/// One batch entry's name and parameters.
fn batch_entry(index: usize, item: &Value) -> Result<Request, ToolOutcome> {
    let entry = index + 1;
    let (name, arguments) = match item {
        Value::String(s) => (s.trim().to_lowercase(), Map::new()),
        Value::Object(map) => {
            let mut lowered = Map::new();
            for (k, v) in map {
                lowered.insert(k.trim().to_lowercase(), v.clone());
            }
            let named = ["name", "indicator"]
                .iter()
                .filter_map(|k| lowered.get(*k))
                .find(|v| truthy(v))
                .map(|v| py_str(v).trim().to_lowercase())
                .unwrap_or_default();
            let field = format!("indicators (entry {})", entry);
            (named, params(lowered.get("params"), &field)?)
        }
        other => return Err(refuse(
            "indicators",
            &format!(
                "entry {} is {}, not a name or an object",
                entry,
                type_name(other)
            ),
            "Every entry is either \"rsi\" or {\"name\": \"rsi\", \"params\": {\"period\": 21}}.",
        )),
    };
    if name.is_empty() {
        return Err(refuse(
            "indicators",
            &format!("entry {} carries no indicator name", entry),
            "Every object entry needs a \"name\", for example {\"name\": \"macd\"}. \"indicator\" \
             is accepted as well.",
        ));
    }
    if get_spec(&name).is_none() {
        return Err(unknown_indicator("indicators", &name));
    }
    Ok((name, arguments))
}

/// `_batch_requests`: up to eight `(name, params)` pairs, an exact repeat
/// of name and parameters computed once.
pub(super) fn batch_requests(indicators: Option<&Value>) -> Result<Vec<Request>, ToolOutcome> {
    let items = match decoded_list(indicators) {
        Some(Value::Object(map)) => vec![Value::Object(map)],
        Some(Value::Array(items)) if !items.is_empty() => items,
        _ => {
            return Err(refuse(
                "indicators",
                "it is empty or is not a list",
                "Pass a list, for example [\"rsi\", {\"name\": \"sma\", \"params\": {\"period\": 50}}].",
            ))
        }
    };
    if items.len() > MAX_BATCH_INDICATORS {
        return Err(refuse(
            "indicators",
            &format!(
                "it carries {} entries, more than the {} one call computes",
                items.len(),
                MAX_BATCH_INDICATORS
            ),
            &format!("Split it into batches of at most {}.", MAX_BATCH_INDICATORS),
        ));
    }
    let mut requests: Vec<Request> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let request = batch_entry(index, item)?;
        if !requests.contains(&request) {
            requests.push(request);
        }
    }
    Ok(requests)
}

/// `_scan_symbols`: up to 15 symbols, upper-cased, repeats dropped.
pub(super) fn scan_symbols(symbols: Option<&Value>) -> Result<Vec<String>, ToolOutcome> {
    let items = match decoded_list(symbols) {
        Some(Value::Array(items)) if !items.is_empty() => items,
        _ => {
            return Err(refuse(
                "symbols",
                "it is empty or is not a list",
                "Pass a list, for example [\"SBIN\", \"RELIANCE\", \"INFY\"].",
            ))
        }
    };
    if items.len() > MAX_SCAN_SYMBOLS {
        return Err(refuse(
            "symbols",
            &format!(
                "it carries {} instruments, more than the {} one scan covers",
                items.len(),
                MAX_SCAN_SYMBOLS
            ),
            &format!(
                "Scan at most {} at a time; each one is a separate broker history call behind a \
                 rate limiter.",
                MAX_SCAN_SYMBOLS
            ),
        ));
    }
    let mut cleaned: Vec<String> = Vec::new();
    for item in &items {
        let text = if truthy(item) {
            py_str(item).trim().to_uppercase()
        } else {
            String::new()
        };
        if !text.is_empty() && !cleaned.contains(&text) {
            cleaned.push(text);
        }
    }
    if cleaned.is_empty() {
        return Err(refuse(
            "symbols",
            "none of the entries is a symbol",
            "Pass a list of OpenAlgo symbols, for example [\"SBIN\", \"RELIANCE\"].",
        ));
    }
    Ok(cleaned)
}
