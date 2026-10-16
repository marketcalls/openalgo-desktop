//! Limits and admission of the research tools.
//!
//! The web bounds none of these inputs; its tool descriptions ask for a
//! modest watchlist ("keep the list modest (≤ ~25)"). The desktop enforces
//! that and the sizes below. Everything is decided before any work starts:
//!
//! 1. Shape: every string, list and map in the arguments is size-capped, and
//!    every count, bar, day and date value is range-checked, with defaults
//!    resolved first so an omitted field is checked as the value it becomes.
//! 2. Expansion: the bars a call would fetch (calendar span of the window
//!    times bars per day of the interval, for every interval and every
//!    symbol) are estimated and refused past MAX_FETCH_BARS per fetch.
//! 3. Concurrency: at most MAX_PER_TOKEN research calls per token and
//!    RESEARCH_SLOTS in the whole process run at once.
//! 4. Budget: the call's whole fan-out (one `/api/v1` call per symbol, per
//!    timeframe, per exchange) is reserved from the token's upstream budget
//!    up front; a call that does not fit is refused without fetching.
//!
//! Nothing retries: a failed fetch is reported, never repeated.

use super::args::{i, lookback, normalize, s};
use super::history::{resolve, Interval};
use crate::mcp::dispatch;
use crate::mcp::envelope::error;
use crate::state::AppState;
use chrono::NaiveDate;
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Symbols in one `screen_instruments` call.
pub const MAX_SCREEN_SYMBOLS: usize = 25;
/// Timeframes in one `multi_timeframe_analysis` call.
pub const MAX_TIMEFRAMES: usize = 8;
/// Columns in `inputs` (no indicator takes more than four series).
pub const MAX_INPUTS: usize = 4;
/// Keys in an indicator's `params`.
pub const MAX_PARAMS: usize = 8;
/// Characters in any string argument.
pub const MAX_STRING: usize = 64;
/// `bars`, `lookback_bars` and `limit` values.
pub const MAX_BARS: i64 = 5000;
/// `lookback_days`, and the span of a date range (explicit, or from
/// `start_date` to today).
pub const MAX_DAYS: i64 = 3660;
/// Estimated bars one history fetch may cover (span times bars a day).
pub const MAX_FETCH_BARS: f64 = 100_000.0;
/// Research calls one token may run at once.
pub const MAX_PER_TOKEN: usize = 2;
/// Research calls running at once (process-wide); a call waits this long
/// for a slot before it is refused.
pub const RESEARCH_SLOTS: usize = 4;
pub const SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// Longest a research call may run in all (its `/api/v1` calls each have
/// their own 120 s limit).
pub const RESEARCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);
/// Longest an admitted call can take from its arrival: the wait for a slot,
/// then its deadline. The stdio bridge waits at least this long for one
/// (TIME-04).
pub const LONGEST_RUN: std::time::Duration =
    std::time::Duration::from_secs(SLOT_WAIT.as_secs() + RESEARCH_DEADLINE.as_secs());

/// The research tools (`mcpserver.py` "RESEARCH TOOLS").
pub const RESEARCH_TOOLS: &[&str] = &[
    "get_historical_data",
    "calculate_indicator",
    "get_trend_snapshot",
    "get_momentum_snapshot",
    "get_volatility_snapshot",
    "get_support_resistance",
    "detect_signals",
    "screen_instruments",
    "multi_timeframe_analysis",
    "correlation_beta",
];

/// Whether a tool is admitted and bounded like a research call (the
/// research tools and `get_instruments`), so it may take up to
/// [`LONGEST_RUN`].
pub fn runs_long(tool: &str) -> bool {
    tool == "get_instruments" || RESEARCH_TOOLS.contains(&tool)
}

/// Run an admitted call to [`RESEARCH_DEADLINE`]; past it, the timeout
/// error with `message`.
pub async fn within_deadline(
    work: impl std::future::Future<Output = Value>,
    message: &str,
) -> Value {
    match tokio::time::timeout(RESEARCH_DEADLINE, work).await {
        Ok(v) => v,
        Err(_) => error(
            message,
            &[
                ("error_type", json!("timeout")),
                ("retry_safe", json!(true)),
            ],
        ),
    }
}

/// Every exchange `get_instruments` reads when none is given.
pub const ALL_INSTRUMENT_EXCHANGES: usize = 9;

pub(super) fn limit_error(message: String) -> Value {
    error(
        message,
        &[
            ("error_type", json!("limit_exceeded")),
            ("retry_safe", json!(true)),
        ],
    )
}

fn busy(message: &str) -> Value {
    error(
        message,
        &[("error_type", json!("busy")), ("retry_safe", json!(true))],
    )
}

/// Strings, lists and maps anywhere in the arguments, size-capped.
fn check_shape(v: &Value, path: &str) -> Option<Value> {
    match v {
        Value::String(t) if t.chars().count() > MAX_STRING => Some(limit_error(format!(
            "'{}' is too long (at most {} characters).",
            path, MAX_STRING
        ))),
        Value::Array(items) => {
            let cap = match path {
                "symbols" => MAX_SCREEN_SYMBOLS,
                "intervals" => MAX_TIMEFRAMES,
                "inputs" => MAX_INPUTS,
                _ => MAX_SCREEN_SYMBOLS,
            };
            if items.len() > cap {
                let what = match path {
                    "symbols" => "symbols per call. Split the watchlist into smaller groups.",
                    "intervals" => "intervals per call.",
                    _ => "entries.",
                };
                return Some(limit_error(format!(
                    "'{}' accepts at most {} {}",
                    path, cap, what
                )));
            }
            items.iter().find_map(|x| check_shape(x, path))
        }
        Value::Object(m) => {
            if m.len() > MAX_PARAMS {
                return Some(limit_error(format!(
                    "'{}' accepts at most {} keys.",
                    path, MAX_PARAMS
                )));
            }
            m.iter().find_map(|(k, x)| {
                if k.chars().count() > MAX_STRING {
                    return Some(limit_error(format!("A key in '{}' is too long.", path)));
                }
                check_shape(x, path)
            })
        }
        _ => None,
    }
}

/// The interval and window size of each fetch the call will make.
fn fetches(tool: &str, a: &Map<String, Value>) -> Vec<(Interval, i64)> {
    let parse = |raw: &str| Interval::parse(raw);
    match tool {
        "get_historical_data" => {
            let bars = i(a, "bars").unwrap_or(20).max(252);
            parse(&s(a, "interval").unwrap_or_default())
                .map(|itv| vec![(itv, bars)])
                .unwrap_or_default()
        }
        "multi_timeframe_analysis" => intervals(a)
            .iter()
            .filter_map(|raw| parse(raw))
            .map(|itv| (itv, lookback(a)))
            .collect(),
        "get_instruments" => Vec::new(),
        _ => parse(&s(a, "interval").unwrap_or_else(|| "D".into()))
            .map(|itv| vec![(itv, lookback(a))])
            .unwrap_or_default(),
    }
}

/// `multi_timeframe_analysis` intervals (the web's default when omitted).
fn intervals(a: &Map<String, Value>) -> Vec<String> {
    a.get("intervals")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.iter()
                .map(|x| x.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_else(|| vec!["5m".into(), "15m".into(), "1h".into(), "D".into()])
}

/// `/api/v1` calls the tool will make: its whole fan-out.
pub fn planned_calls(tool: &str, a: &Map<String, Value>) -> usize {
    let n_list = |k: &str| a.get(k).and_then(Value::as_array).map_or(0, Vec::len);
    match tool {
        "screen_instruments" => n_list("symbols"),
        "multi_timeframe_analysis" => intervals(a).len(),
        "correlation_beta" => 2,
        "get_instruments" => {
            if a.get("exchange").is_some_and(|e| !e.is_null()) {
                1
            } else {
                ALL_INSTRUMENT_EXCHANGES
            }
        }
        _ => 1,
    }
}

/// Check the arguments against every limit and return their canonical
/// form, which is all the tool reads afterwards: the windows checked here
/// are the windows fetched.
pub fn check_limits(
    tool: &str,
    a: &Map<String, Value>,
    today: NaiveDate,
) -> Result<Map<String, Value>, Value> {
    for (k, v) in a {
        if let Some(e) = check_shape(v, k) {
            return Err(e);
        }
    }
    let norm = normalize(a)?;
    for (itv, lookback_bars) in fetches(tool, &norm) {
        resolve(&norm, &itv, lookback_bars, today).map_err(limit_error)?;
    }
    Ok(norm)
}

/// Holds a research call's slots for as long as it runs.
pub struct Admission {
    _token: Option<crate::mcp::http::InflightGuard>,
    _slot: tokio::sync::OwnedSemaphorePermit,
    /// The canonical arguments (see [`normalize`]).
    pub args: Map<String, Value>,
    /// The day every window of this call is resolved against.
    pub today: NaiveDate,
}

/// Admit a call before any work: limits, then the token's in-flight cap,
/// then a process-wide slot, then its whole upstream fan-out reserved from
/// the token's budget. `Err` is the tool output refusing it.
pub async fn admit(
    ctx: &Arc<AppState>,
    tool: &str,
    a: &Map<String, Value>,
) -> Result<Admission, Value> {
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    let args = check_limits(tool, a, today)?;
    let a = &args;
    let budget = dispatch::CALLER.try_with(|b| b.clone()).ok();
    let token =
        match &budget {
            Some(b) => match ctx.mcp.enter(&b.key, MAX_PER_TOKEN) {
                Some(g) => Some(g),
                None => return Err(busy(
                    "This AI client already has the most analysis requests OpenAlgo runs at once. \
Wait for one to finish and try again.",
                )),
            },
            None => None,
        };
    let slots = ctx.mcp.research_slots();
    let Ok(Ok(slot)) = tokio::time::timeout(SLOT_WAIT, slots.acquire_owned()).await else {
        return Err(busy(
            "OpenAlgo is busy with other analysis requests, so this one was not run. Try again in a moment.",
        ));
    };
    if let Some(b) = &budget {
        if !b.reserve(ctx, planned_calls(tool, a)) {
            return Err(error(
                "This AI client has made too many requests to OpenAlgo in the last minute. \
Wait a minute and try again.",
                &[
                    ("error_type", json!("rate_limited")),
                    ("retry_safe", json!(true)),
                ],
            ));
        }
    }
    Ok(Admission {
        _token: token,
        _slot: slot,
        args,
        today,
    })
}
