//! The chart arguments a model spells several ways: indicators, the chart
//! shape and the expiries of a volatility surface (the web's
//! `normalise_indicators`, `_indicator`, `_indicator_inputs`, `_chart_type`
//! and `_expiries` in `tools/viz.py`).
//!
//! Every refusal names the argument and what a valid value looks like, so
//! the model corrects itself in one retry.

use crate::agent::chart_catalogue;
use crate::agent::contract::tool::ToolOutcome;
use crate::agent::tools::market::normalise::refuse;
use crate::agent::tools::options::normalise_expiry;
use crate::agent::tools::values::{py_repr, py_str, type_name, Pattern};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};

/// Most indicators one price chart may carry.
pub const MAX_INDICATORS: usize = 6;

/// Most inputs one indicator may declare.
pub const MAX_INDICATOR_INPUTS: usize = 8;

/// Most expiries one volatility surface may span.
pub const MAX_SURFACE_EXPIRIES: usize = 8;

/// Close matches named when an indicator id is refused.
const MAX_SUGGESTIONS: usize = 6;

/// The input an `openalgo-charts` moving average and most oscillators call
/// their period.
const LENGTH_KEY: &str = "length";

/// The shapes `/trading` draws, so the chat chart and the terminal chart
/// are the same picture.
pub const CHART_TYPES: [&str; 9] = [
    "candlestick",
    "hollow-candle",
    "bar",
    "high-low",
    "line",
    "step",
    "area",
    "baseline",
    "heikin-ashi",
];

/// The shape when the model names none.
pub const DEFAULT_CHART_TYPE: &str = "candlestick";

static INDICATOR_ID: Pattern = Pattern::new(r"\A[a-z][a-z0-9-]{1,31}\z");
static INPUT_KEY: Pattern = Pattern::new(r"\A[A-Za-z][A-Za-z0-9_]{0,23}\z");
/// `ema:20` and `ema(20)`, the two shorthands a model reaches for.
static SHORTHAND: Pattern =
    Pattern::new(r"\A([A-Za-z][A-Za-z0-9_-]{1,31})\s*[:(]?\s*(\d{1,4})?\)?\z");
static LIST_SPLIT: Pattern = Pattern::new(r"[,\n]");
static EXPIRY_SPLIT: Pattern = Pattern::new(r"[,\s]+");

/// Names other charting tools use for an indicator the chart calls
/// something else (`bbands` is also the `openalgo.ta` name).
const ALIASES: [(&str, &str); 4] = [
    ("bbands", "bollinger"),
    ("bb", "bollinger"),
    ("psar", "parabolic-sar"),
    ("sar", "parabolic-sar"),
];

const COMMON_IDS: &str = "Use a lower-case id such as 'ema', 'sma', 'rsi', 'macd', 'bollinger', 'vwap', 'atr' or 'supertrend'.";

fn indicators_refused(reason: &str, fix: &str) -> ToolOutcome {
    refuse("indicators", reason, fix)
}

/// Python's truthiness of a decoded JSON value.
fn truthy(value: &Value) -> bool {
    crate::agent::tools::values::truthy(value)
}

/// One indicator's inputs: scalar values under identifier keys, strings
/// trimmed to 32 characters; anything nested or unnamed is dropped.
fn indicator_inputs(raw: Option<&Value>) -> Result<Map<String, Value>, ToolOutcome> {
    let Some(Value::Object(raw)) = raw else {
        return Ok(Map::new());
    };
    if raw.len() > MAX_INDICATOR_INPUTS {
        return Err(indicators_refused(
            &format!(
                "one indicator declares {} inputs, more than the {} allowed",
                raw.len(),
                MAX_INDICATOR_INPUTS
            ),
            "Name only the inputs you are changing; the chart supplies its own defaults for the rest.",
        ));
    }
    let mut inputs = Map::new();
    for (key, value) in raw {
        let name = key.trim();
        if !INPUT_KEY.is_match(name) {
            continue;
        }
        match value {
            Value::Bool(_) | Value::Number(_) => {
                inputs.insert(name.into(), value.clone());
            }
            Value::String(s) if !s.trim().is_empty() => {
                inputs.insert(
                    name.into(),
                    json!(s.trim().chars().take(32).collect::<String>()),
                );
            }
            _ => {}
        }
    }
    Ok(inputs)
}

/// Python's `int()` of a length a model sent as a number (a bool is an int
/// there too).
fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        _ => None,
    }
}

/// The id and inputs an object entry names.
fn object_entry(item: &Map<String, Value>) -> Result<(String, Map<String, Value>), ToolOutcome> {
    let lowered: HashMap<String, &Value> = item
        .iter()
        .map(|(k, v)| (k.trim().to_lowercase(), v))
        .collect();
    let pick = |names: &[&str]| -> Option<&Value> {
        names
            .iter()
            .filter_map(|n| lowered.get(*n).copied())
            .find(|v| truthy(v))
    };
    let identifier = pick(&["id", "name", "indicator"])
        .map(py_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let mut inputs = indicator_inputs(lowered.get("inputs").copied())?;
    if !inputs.contains_key(LENGTH_KEY) {
        if let Some(length) = pick(&[LENGTH_KEY, "period"]).and_then(whole) {
            inputs.insert(LENGTH_KEY.into(), json!(length));
        }
    }
    Ok((identifier, inputs))
}

/// One indicator request as `{"id": ..., "inputs": {...}}`.
fn indicator(item: &Value, position: usize) -> Result<Value, ToolOutcome> {
    let (identifier, inputs) = match item {
        Value::Object(map) => object_entry(map)?,
        Value::String(text) => {
            let Some(c) = SHORTHAND.captures(text.trim()) else {
                return Err(indicators_refused(
                    &format!("entry {} is {}, which is not an indicator", position, py_repr(item)),
                    "Use 'ema:20', or an object such as {\"id\": \"supertrend\", \"inputs\": {\"length\": 10, \"multiplier\": 3}}.",
                ));
            };
            let mut inputs = Map::new();
            if let Some(n) = c.get(2).and_then(|m| m.as_str().parse::<i64>().ok()) {
                inputs.insert(LENGTH_KEY.into(), json!(n));
            }
            (c[1].to_lowercase(), inputs)
        }
        other => {
            return Err(indicators_refused(
                &format!(
                    "entry {} is {}, not an indicator name or object",
                    position,
                    type_name(other)
                ),
                "Pass strings such as \"ema:20\", or objects carrying an \"id\".",
            ))
        }
    };
    let dashed = identifier.replace('_', "-");
    let identifier = ALIASES
        .iter()
        .find(|(from, _)| *from == dashed)
        .map(|(_, to)| to.to_string())
        .unwrap_or(dashed);
    if !INDICATOR_ID.is_match(&identifier) {
        let named = if identifier.is_empty() {
            "nothing"
        } else {
            &identifier
        };
        return Err(indicators_refused(
            &format!(
                "entry {} names {}, which is not an indicator id",
                position, named
            ),
            COMMON_IDS,
        ));
    }
    known_to_the_chart(&identifier, position, chart_catalogue::ids())?;
    Ok(json!({"id": identifier, "inputs": inputs}))
}

/// Refuse an id the chart cannot draw, naming the nearest ones it can. An
/// empty catalogue checks the shape only (the web's unreadable-file path).
fn known_to_the_chart(
    identifier: &str,
    position: usize,
    known: &BTreeSet<String>,
) -> Result<(), ToolOutcome> {
    if known.is_empty() || known.contains(identifier) {
        return Ok(());
    }
    let close = close_indicator_ids(identifier, known);
    let lead = if close.is_empty() {
        "Common ids are 'ema', 'sma', 'rsi', 'macd', 'bollinger', 'vwap', 'atr' and 'supertrend'. "
            .to_string()
    } else {
        format!("The closest ids the chart has are: {}. ", close.join(", "))
    };
    Err(indicators_refused(
        &format!(
            "entry {} names {}, which this chart cannot draw",
            position,
            py_repr(&json!(identifier))
        ),
        &format!(
            "{}Use one exactly as written, or leave the indicator out.",
            lead
        ),
    ))
}

/// Up to six catalogue ids near a refused one: those sharing its first word
/// or containing it first, then the closest spellings.
fn close_indicator_ids(identifier: &str, known: &BTreeSet<String>) -> Vec<String> {
    let ordered: Vec<&str> = known.iter().map(String::as_str).collect();
    let root = identifier.split('-').next().unwrap_or(identifier);
    let related = ordered.iter().copied().filter(|item| {
        item.starts_with(root)
            || item.contains(identifier)
            || (item.chars().count() >= 3 && root.contains(item))
    });
    let spelled = get_close_matches(identifier, &ordered, MAX_SUGGESTIONS, 0.6);
    let mut out: Vec<String> = Vec::new();
    for item in related.chain(spelled) {
        if !out.iter().any(|o| o == item) {
            out.push(item.to_string());
        }
    }
    out.truncate(MAX_SUGGESTIONS);
    out
}

/// Python's `difflib.get_close_matches`: the `n` candidates whose
/// `SequenceMatcher` ratio against `word` is at least `cutoff`, best first,
/// ties by the candidate in reverse order (`heapq.nlargest` on tuples).
fn get_close_matches<'a>(
    word: &str,
    candidates: &[&'a str],
    n: usize,
    cutoff: f64,
) -> Vec<&'a str> {
    let b: Vec<char> = word.chars().collect();
    let mut scored: Vec<(f64, &'a str)> = candidates
        .iter()
        .map(|c| (ratio(&c.chars().collect::<Vec<_>>(), &b), *c))
        .filter(|(score, _)| *score >= cutoff)
        .collect();
    scored.sort_by(|x, y| y.0.total_cmp(&x.0).then_with(|| y.1.cmp(x.1)));
    scored.into_iter().take(n).map(|(_, c)| c).collect()
}

/// `SequenceMatcher(None, a, b).ratio()`: twice the matched characters over
/// the total length. No junk, and `b` is far below the 200 characters
/// where difflib's popularity heuristic starts.
fn ratio(a: &[char], b: &[char]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, c) in b.iter().enumerate() {
        b2j.entry(*c).or_default().push(j);
    }
    let mut matched = 0usize;
    let mut queue = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, &b2j, (alo, ahi, blo, bhi));
        if k > 0 {
            matched += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    2.0 * matched as f64 / total as f64
}

/// `SequenceMatcher.find_longest_match` without junk: the earliest longest
/// block in `a[alo..ahi]` and `b[blo..bhi]`.
fn longest_match(
    a: &[char],
    b2j: &HashMap<char, Vec<usize>>,
    (alo, ahi, blo, bhi): (usize, usize, usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, c) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut next: HashMap<usize, usize> = HashMap::new();
        for &j in b2j.get(c).map(Vec::as_slice).unwrap_or(&[]) {
            if j < blo {
                continue;
            }
            if j >= bhi {
                break;
            }
            let k = j
                .checked_sub(1)
                .and_then(|p| j2len.get(&p))
                .copied()
                .unwrap_or(0)
                + 1;
            next.insert(j, k);
            if k > bestsize {
                besti = i + 1 - k;
                bestj = j + 1 - k;
                bestsize = k;
            }
        }
        j2len = next;
    }
    (besti, bestj, bestsize)
}

/// A list argument a model may have sent as text: a JSON array or object,
/// or a comma or newline separated string.
fn list_entries(value: &Value, field: &str, example: &str) -> Result<Vec<Value>, ToolOutcome> {
    let raw = match value {
        Value::String(s) if s.trim().starts_with('[') || s.trim().starts_with('{') => {
            serde_json::from_str::<Value>(s.trim()).map_err(|_| {
                refuse(
                    field,
                    "it is a string that is not valid JSON",
                    &format!("Pass a list, for example {}.", example),
                )
            })?
        }
        Value::String(s) => Value::Array(
            LIST_SPLIT
                .split(s.trim())
                .into_iter()
                .filter(|p| !p.trim().is_empty())
                .map(|p| json!(p))
                .collect(),
        ),
        other => other.clone(),
    };
    match raw {
        Value::Object(_) => Ok(vec![raw]),
        Value::Array(items) => Ok(items),
        other => Err(refuse(
            field,
            &format!("it is {}, not a list", type_name(&other)),
            "Pass a list, for example [\"ema:20\", \"ema:50\"].",
        )),
    }
}

/// The `indicators` argument: at most [`MAX_INDICATORS`] descriptors
/// `{"id", "inputs"}`, empty when none were asked for.
pub fn normalise_indicators(value: Option<&Value>) -> Result<Vec<Value>, ToolOutcome> {
    let value = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => return Ok(Vec::new()),
        Some(v) => v,
    };
    let raw = list_entries(value, "indicators", "[\"ema:20\", \"rsi:14\"]")?;
    if raw.len() > MAX_INDICATORS {
        return Err(indicators_refused(
            &format!(
                "it carries {} entries, more than the {} one chart shows",
                raw.len(),
                MAX_INDICATORS
            ),
            "Ask for the few that answer the question; a chart carrying more is unreadable.",
        ));
    }
    raw.iter()
        .enumerate()
        .map(|(i, item)| indicator(item, i + 1))
        .collect()
}

/// `ema(20)` where a length was given, else the id.
pub fn overlay_label(indicator: &Value) -> String {
    let id = indicator.get("id").map(py_str).unwrap_or_default();
    match indicator
        .get("inputs")
        .and_then(|i| i.get(LENGTH_KEY))
        .filter(|l| truthy(l))
    {
        Some(length) => format!("{}({})", id, py_str(length)),
        None => id,
    }
}

/// The price chart shape, one of [`CHART_TYPES`].
pub fn chart_type(value: Option<&Value>) -> Result<&'static str, ToolOutcome> {
    let given = value.filter(|v| truthy(v)).map(py_str);
    let mut text = given
        .unwrap_or_else(|| DEFAULT_CHART_TYPE.to_string())
        .trim()
        .to_lowercase()
        .replace('_', "-");
    if text == "candle" || text == "candles" {
        text = "candlestick".into();
    }
    CHART_TYPES
        .iter()
        .find(|t| **t == text)
        .copied()
        .ok_or_else(|| {
            refuse(
                "chart_type",
                &format!(
                    "{} is not a chart shape this terminal draws",
                    py_repr(&json!(text))
                ),
                &format!("Use one of: {}.", CHART_TYPES.join(", ")),
            )
        })
}

/// The `expiry_dates` of a volatility surface: DDMMMYY, de-duplicated, in
/// the order given, at most [`MAX_SURFACE_EXPIRIES`].
pub fn expiries(value: Option<&Value>) -> Result<Vec<String>, ToolOutcome> {
    const FIELD: &str = "expiry_dates";
    const EXAMPLE: &str = "[\"28NOV25\", \"26DEC25\"]";
    let raw = match value.unwrap_or(&Value::Null) {
        Value::String(s) if s.trim().starts_with('[') => serde_json::from_str::<Value>(s.trim())
            .map_err(|_| {
                refuse(
                    FIELD,
                    "it is a string that is not valid JSON",
                    &format!("Pass a list, for example {}.", EXAMPLE),
                )
            })?,
        Value::String(s) => Value::Array(
            EXPIRY_SPLIT
                .split(s.trim())
                .into_iter()
                .filter(|p| !p.is_empty())
                .map(|p| json!(p))
                .collect(),
        ),
        other => other.clone(),
    };
    let items = match raw {
        Value::Array(items) if !items.is_empty() => items,
        _ => {
            return Err(refuse(
                FIELD,
                "it is empty or is not a list",
                &format!("Pass at least one expiry, for example {}. Look the listed expiries up first rather than guessing them.", EXAMPLE),
            ))
        }
    };
    if items.len() > MAX_SURFACE_EXPIRIES {
        return Err(refuse(
            FIELD,
            &format!(
                "it carries {} expiries, more than the {} one surface spans",
                items.len(),
                MAX_SURFACE_EXPIRIES
            ),
            "Ask for the near expiries that answer the question.",
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for item in &items {
        let expiry = normalise_expiry(Some(item), FIELD)?;
        if !out.contains(&expiry) {
            out.push(expiry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
