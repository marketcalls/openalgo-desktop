//! The shape of an indicator answer (the web's `IndicatorsToolkit._shared`,
//! `_table` and `_trimmed`): the instrument block with the candle tail every
//! indicator on that instrument shares, and each computed result narrowed to
//! the same tail. Built as [`Json`] so the keys reach the model in the web's
//! order (a MACD table lists `macd_line`, `signal_line`, `histogram`).

use crate::agent::contract::tool::{ToolOutcome, MAX_RESULT_CHARS};
use crate::agent::indicators::{ist_stamp, Computed, Json};
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::redact::redact;
use crate::agent::tools::answer::{json_result, Label};
use crate::agent::tools::market::candles::{fit_to_budget, CandleFrame};
use serde_json::Value;
use std::cell::Cell;

/// What every result on one instrument repeats.
pub(super) struct Shared<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub interval: &'a str,
    pub source: &'a str,
    /// Rows the history service returned, 0 when the run's copy was reused.
    pub bars_fetched: usize,
    pub notices: &'a [String],
}

/// Each notice once, first occurrence first (`list(dict.fromkeys(...))`).
pub(super) fn unique(notices: &[String]) -> Json {
    let mut out: Vec<String> = Vec::new();
    for n in notices {
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    Json::Arr(out.into_iter().map(Json::Str).collect())
}

/// `_shared`: the instrument, the window and the last `limit` candles.
pub(super) fn shared(frame: &CandleFrame, limit: usize, head: &Shared<'_>) -> Json {
    let start = frame.len().saturating_sub(limit);
    let candles: Vec<Json> = (start..frame.len())
        .map(|i| {
            let mut candle = Json::obj([("timestamp", Json::str(ist_stamp(frame.time[i])))]);
            for (column, values) in &frame.columns {
                candle.set(column, Json::Num(values[i]));
            }
            candle
        })
        .collect();
    let timestamps: Vec<Json> = candles
        .iter()
        .filter_map(|c| c.get("timestamp").cloned())
        .collect();
    let returned = candles.len();
    let mut payload = Json::obj([
        ("symbol", Json::str(head.symbol)),
        ("exchange", Json::str(head.exchange)),
        ("interval", Json::str(head.interval)),
        ("source", Json::str(head.source)),
        ("timezone", Json::str("Asia/Kolkata")),
        ("bars_computed_over", Json::Int(frame.len() as i64)),
        ("bars_fetched", Json::Int(head.bars_fetched as i64)),
        ("values_returned", Json::Int(returned as i64)),
        ("timestamps", Json::Arr(timestamps)),
        ("candles", Json::Arr(candles)),
    ]);
    if frame.len() > returned {
        payload.set(
            "note",
            Json::str(format!(
                "Every value was computed over all {} bars; the {} most recent are returned, \
                 oldest first. Raise last_n for a longer table.",
                frame.len(),
                returned
            )),
        );
    }
    if !head.notices.is_empty() {
        payload.set("notices", unique(head.notices));
    }
    payload
}

/// `_trimmed`: one result without its own timestamps (the shared block has
/// them) and with each output cut to the last `limit` values.
pub(super) fn trimmed(result: &Computed, limit: usize) -> Json {
    let Json::Obj(pairs) = result.to_json() else {
        return Json::Obj(Vec::new());
    };
    let mut out = Json::Obj(
        pairs
            .into_iter()
            .filter(|(k, _)| k != "timestamps")
            .collect(),
    );
    let mut longest = 0;
    if let Some(Json::Obj(series)) = out.get("values").cloned() {
        let cut: Vec<(String, Json)> = series
            .into_iter()
            .map(|(label, values)| {
                let items = match values {
                    Json::Arr(items) => items,
                    _ => Vec::new(),
                };
                let kept = items[items.len().saturating_sub(limit)..].to_vec();
                longest = longest.max(kept.len());
                (label, Json::Arr(kept))
            })
            .collect();
        out.set("values", Json::Obj(cut));
    }
    out.set("values_returned", Json::Int(longest as i64));
    out
}

/// `_table`: the shared block with one result merged in; a key both carry
/// keeps the shared block's place (`dict.update`).
pub(super) fn table(
    result: &Computed,
    frame: &CandleFrame,
    limit: usize,
    head: &Shared<'_>,
) -> Json {
    let mut payload = shared(frame, limit, head);
    if let Json::Obj(pairs) = trimmed(result, limit) {
        for (key, value) in pairs {
            payload.set(&key, value);
        }
    }
    payload
}

/// [`fit_to_budget`] for an ordered payload: the largest row count whose
/// rendering fits the character budget (the length does not depend on the
/// key order, so the shared measure decides).
pub(super) fn fit(build: impl Fn(usize) -> Json, start: usize) -> Json {
    let chosen = Cell::new(start);
    let _ = fit_to_budget(
        |limit| {
            chosen.set(limit);
            serde_json::to_value(build(limit)).unwrap_or(Value::Null)
        },
        start,
    );
    build(chosen.get())
}

/// A result as the model reads it, in the web's key order: labelled as
/// data like every tool result. When redaction would change anything, or
/// the text is over the cap, the shared path answers instead (redacted and
/// capped, keys sorted), so an ordered answer never skips either.
pub(super) fn answer(tool: &str, payload: &Json, labels: &[Label<'_>]) -> ToolOutcome {
    let value = serde_json::to_value(payload).unwrap_or(Value::Null);
    let text = payload.to_text();
    if redact(&value) != value || text.chars().count() > MAX_RESULT_CHARS {
        return json_result(tool, &value, labels);
    }
    let borrowed: Vec<(&str, Option<&str>)> =
        labels.iter().map(|(k, v)| (*k, v.as_deref())).collect();
    ToolOutcome::Ok(wrap_tool_result(tool, &text, &borrowed))
}
