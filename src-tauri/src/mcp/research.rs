//! The research tools (`mcpserver.py` "RESEARCH TOOLS"): history through
//! `/api/v1/history` shaped as the SDK's DataFrame, indicators from
//! [`super::ta`], and the web's summaries (`_load_history`, `_last`,
//! `_bundle`, `_df_records`).

use super::dispatch;
use super::envelope::{error, fail, py_repr};
use super::ta::{self, Out, PyErr};
use crate::state::AppState;
use chrono::{Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Rows of history indexed by timestamp, as the SDK's `history()` frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Epoch seconds, ascending, unique.
    pub ts: Vec<i64>,
    /// Intraday frames carry IST-aware timestamps; daily ones are naive.
    pub intraday: bool,
    /// Every other column, in the reply's key order.
    pub cols: Vec<(String, Vec<Value>)>,
}

impl Frame {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    /// A column as floats (`df[name]`), or the KeyError pandas raises.
    pub fn col(&self, name: &str) -> Result<Vec<f64>, PyErr> {
        self.cols
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect())
            .ok_or_else(|| PyErr {
                kind: "KeyError",
                message: format!("'{}'", name),
            })
    }

    fn close(&self) -> Vec<f64> {
        self.col("close").unwrap_or_default()
    }

    /// The last `n` rows (`df.tail(n)`).
    pub fn tail(&self, n: usize) -> Frame {
        let start = self.len().saturating_sub(n);
        Frame {
            ts: self.ts[start..].to_vec(),
            intraday: self.intraday,
            cols: self
                .cols
                .iter()
                .map(|(k, v)| (k.clone(), v[start..].to_vec()))
                .collect(),
        }
    }

    /// `pandas.Timestamp.isoformat()` of row `i` (web `_idx_iso`).
    pub fn iso(&self, i: usize) -> String {
        iso(self.ts[i], self.intraday)
    }
}

fn iso(ts: i64, intraday: bool) -> String {
    let utc = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    if intraday {
        utc.with_timezone(&Kolkata)
            .format("%Y-%m-%dT%H:%M:%S%:z")
            .to_string()
    } else {
        utc.naive_utc().format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// `DataFrame.to_json(date_format="iso")` timestamp: UTC with
/// milliseconds, `Z` when the index is timezone-aware.
fn json_iso(ts: i64, intraday: bool) -> String {
    let utc = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    let s = utc.naive_utc().format("%Y-%m-%dT%H:%M:%S%.3f").to_string();
    if intraday {
        format!("{}Z", s)
    } else {
        s
    }
}

/// A float as `to_json` writes it (10 decimal places, NaN as null).
fn json_float(x: f64) -> Value {
    if x.is_finite() {
        json!(ta::py_round(x, 10))
    } else {
        Value::Null
    }
}

/// Python slice `seq[-n:]` start index for a list of `len`.
fn tail_start(len: usize, n: i64) -> usize {
    if n == 0 {
        0
    } else if n > 0 {
        len.saturating_sub(n as usize)
    } else {
        (n.unsigned_abs() as usize).min(len)
    }
}

/// Web `_df_records(df, limit)`: the last `limit` rows (all when 0) as
/// records with a `timestamp` key.
pub fn records(
    ts: &[i64],
    intraday: bool,
    cols: &[(String, Vec<Value>)],
    limit: i64,
) -> Vec<Value> {
    let start = if limit == 0 {
        0
    } else {
        tail_start(ts.len(), limit)
    };
    (start..ts.len())
        .map(|i| {
            let mut m = Map::new();
            m.insert("timestamp".into(), json!(json_iso(ts[i], intraday)));
            for (k, v) in cols {
                let cell = match &v[i] {
                    Value::Number(n) if n.is_f64() => json_float(n.as_f64().unwrap_or(f64::NAN)),
                    other => other.clone(),
                };
                m.insert(k.clone(), cell);
            }
            Value::Object(m)
        })
        .collect()
}

fn f64_col(v: &[f64]) -> Vec<Value> {
    v.iter().map(|x| json!(*x)).collect()
}

fn opt(x: Option<f64>) -> Value {
    x.map(|v| json!(v)).unwrap_or(Value::Null)
}

/// Build the frame from the SDK reply's `data` (sorted, duplicates dropped).
fn frame_from(data: &[Value], intraday: bool) -> Frame {
    let mut rows: Vec<(i64, &Map<String, Value>)> = data
        .iter()
        .filter_map(|r| {
            let o = r.as_object()?;
            let t = o.get("timestamp")?;
            let ts = t.as_i64().or_else(|| t.as_f64().map(|f| f as i64))?;
            Some((ts, o))
        })
        .collect();
    rows.sort_by_key(|(t, _)| *t);
    rows.dedup_by_key(|(t, _)| *t);
    let mut names: Vec<String> = Vec::new();
    if let Some((_, first)) = rows.first() {
        for k in first.keys() {
            if k != "timestamp" {
                names.push(k.clone());
            }
        }
    }
    let cols = names
        .into_iter()
        .map(|n| {
            let vals: Vec<Value> = rows
                .iter()
                .map(|(_, o)| o.get(&n).cloned().unwrap_or(Value::Null))
                .collect();
            // A column holding any float is a float column in pandas.
            let float = vals
                .iter()
                .any(|v| v.as_number().is_some_and(|x| x.is_f64()));
            let vals = if float {
                vals.into_iter()
                    .map(|v| v.as_f64().map(|f| json!(f)).unwrap_or(Value::Null))
                    .collect()
            } else {
                vals
            };
            (n, vals)
        })
        .collect();
    Frame {
        ts: rows.iter().map(|(t, _)| *t).collect(),
        intraday,
        cols,
    }
}

/// Web `_history_df`: the SDK's `history()` then the two refusals.
async fn history_df(
    ctx: &Arc<AppState>,
    symbol: &str,
    exchange: &str,
    interval: &str,
    start: &str,
    end: &str,
    source: &str,
) -> Result<Frame, PyErr> {
    let mut p = Map::new();
    p.insert("symbol".into(), json!(symbol.to_uppercase()));
    p.insert("exchange".into(), json!(exchange.to_uppercase()));
    p.insert("interval".into(), json!(interval));
    p.insert("start_date".into(), json!(start));
    p.insert("end_date".into(), json!(end));
    p.insert("source".into(), json!(source));
    let result = dispatch::sdk_post(ctx, "history", p).await;
    let ok = result.get("status").and_then(Value::as_str) == Some("success");
    let reply = match (ok, result.get("data").and_then(Value::as_array)) {
        (true, Some(rows)) if rows.is_empty() => json!({
            "status": "error",
            "message": "No data available for the specified period",
            "error_type": "no_data",
        }),
        (true, Some(rows)) => {
            let f = frame_from(rows, !matches!(interval, "D" | "W" | "M"));
            if f.is_empty() {
                return Err(PyErr::value(
                    "no historical data returned for the given range",
                ));
            }
            return Ok(f);
        }
        _ => result,
    };
    Err(PyErr::value(format!("history error: {}", py_repr(&reply))))
}

/// Approximate bars per trading day (`_BARS_PER_DAY`).
fn bars_per_day(interval: &str) -> f64 {
    match interval.to_lowercase().as_str() {
        "1m" => 375.0,
        "3m" => 125.0,
        "5m" => 75.0,
        "10m" => 38.0,
        "15m" => 25.0,
        "30m" => 13.0,
        "1h" | "60m" => 7.0,
        "2h" => 4.0,
        "3h" => 3.0,
        "4h" => 2.0,
        "d" | "day" => 1.0,
        "w" | "week" => 0.2,
        "m" | "month" => 0.05,
        _ => 75.0,
    }
}

fn parse_iso_date(s: &str) -> Result<NaiveDate, PyErr> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| PyErr::value(format!("Invalid isoformat string: '{}'", s)))
}

/// The window a research call asks for.
#[derive(Debug, Clone)]
pub struct Window {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub lookback_bars: i64,
    pub lookback_days: Option<i64>,
    pub source: String,
}

/// Web `_load_history`: explicit range, else the last N days, else enough
/// calendar days for `lookback_bars` bars, trimmed to them.
pub async fn load_history(
    ctx: &Arc<AppState>,
    symbol: &str,
    exchange: &str,
    interval: &str,
    w: &Window,
) -> Result<Frame, PyErr> {
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    let end = match w.end_date.as_deref() {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => today.format("%Y-%m-%d").to_string(),
    };
    if let Some(s) = w.start_date.as_deref().filter(|s| !s.is_empty()) {
        return history_df(ctx, symbol, exchange, interval, s, &end, &w.source).await;
    }
    if let Some(days) = w.lookback_days.filter(|d| *d != 0) {
        let start = parse_iso_date(&end)? - ChronoDuration::days(days);
        let start = start.format("%Y-%m-%d").to_string();
        return history_df(ctx, symbol, exchange, interval, &start, &end, &w.source).await;
    }
    let cal_days = ((w.lookback_bars as f64 / bars_per_day(interval)) * 1.6) as i64 + 5;
    let start = (parse_iso_date(&end)? - ChronoDuration::days(cal_days))
        .format("%Y-%m-%d")
        .to_string();
    let f = history_df(ctx, symbol, exchange, interval, &start, &end, &w.source).await?;
    Ok(if w.lookback_bars >= 0 {
        f.tail(w.lookback_bars as usize)
    } else {
        f
    })
}

// ----------------------------------------------------------------------
// Argument helpers
// ----------------------------------------------------------------------

fn s(a: &Map<String, Value>, k: &str) -> Option<String> {
    a.get(k).and_then(Value::as_str).map(str::to_string)
}

fn i(a: &Map<String, Value>, k: &str) -> Option<i64> {
    match a.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn f(a: &Map<String, Value>, k: &str) -> Option<f64> {
    match a.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn window(a: &Map<String, Value>) -> Window {
    Window {
        start_date: s(a, "start_date"),
        end_date: s(a, "end_date"),
        lookback_bars: i(a, "lookback_bars").unwrap_or(252),
        lookback_days: i(a, "lookback_days"),
        source: s(a, "source").unwrap_or_else(|| "api".into()),
    }
}

const HLC_INDICATORS: &[&str] = &[
    "atr",
    "natr",
    "true_range",
    "adx",
    "adxr",
    "dmi",
    "dx",
    "supertrend",
    "stochastic",
    "stochf",
    "cci",
    "williams_r",
    "keltner",
    "donchian",
    "aroon",
    "aroon_oscillator",
    "psar",
    "ichimoku",
    "pivot_points",
    "ultimate_oscillator",
    "uo_oscillator",
    "chandelier_exit",
    "starc",
    "elderray",
    "ckstop",
    "fractals",
    "rwi",
    "alligator",
    "gator_oscillator",
    "bop",
    "rvi",
    "fisher",
    "avgprice",
    "medprice",
    "midprice",
    "typprice",
    "wclprice",
];
const HLCV_INDICATORS: &[&str] = &["mfi", "cmf", "adl", "emv", "klingervolumeoscillator"];

/// Web `_resolve_inputs`.
fn resolve_inputs(
    df: &Frame,
    name: &str,
    inputs: Option<&Vec<Value>>,
) -> Result<(Vec<String>, Vec<Vec<f64>>), PyErr> {
    let cols: Vec<String> = match inputs.filter(|v| !v.is_empty()) {
        Some(v) => v
            .iter()
            .map(|c| c.as_str().unwrap_or_default().to_lowercase())
            .collect(),
        None if HLCV_INDICATORS.contains(&name) => {
            vec!["high".into(), "low".into(), "close".into(), "volume".into()]
        }
        None if HLC_INDICATORS.contains(&name) => vec!["high".into(), "low".into(), "close".into()],
        None => vec!["close".into()],
    };
    let series = cols
        .iter()
        .map(|c| df.col(c))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((cols, series))
}

/// The web's answer for an indicator the desktop does not compute yet.
fn not_available(name: &str) -> Value {
    error(
        format!(
            "The '{}' indicator is not available in OpenAlgo Desktop yet. Indicators available here: {}.",
            name,
            ta::supported().join(", ")
        ),
        &[("error_type", json!("not_available"))],
    )
}

/// Name check shared by calculate_indicator and multi_timeframe_analysis.
fn check_indicator(indicator: &str) -> Result<String, Value> {
    let name = indicator.to_lowercase();
    if !ta::WEB_TA_FUNCTIONS.contains(&name.as_str()) {
        return Err(error(format!("unknown indicator '{}'", indicator), &[]));
    }
    if !ta::is_supported(&name) {
        return Err(not_available(&name));
    }
    Ok(name)
}

/// `_last` of a result: one value, or a list for a tuple result.
fn latest(out: &Out) -> Value {
    match out {
        Out::One(v) => opt(ta::last(v)),
        Out::Many(v) => Value::Array(v.iter().map(|s| opt(ta::last(s))).collect()),
    }
}

/// Web `_bundle`: latest values, an error object per failing item.
fn bundle(items: Vec<(&str, Result<Out, PyErr>)>) -> Value {
    let mut m = Map::new();
    for (k, r) in items {
        let v = match r {
            Ok(o) => latest(&o),
            Err(e) => json!({"error": e.message}),
        };
        m.insert(k.into(), v);
    }
    Value::Object(m)
}

fn no_params() -> Map<String, Value> {
    Map::new()
}

fn kw(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

type ToolResult = Result<Value, (String, PyErr)>;

fn tag(action: &str) -> impl Fn(PyErr) -> (String, PyErr) + '_ {
    move |e| (action.to_string(), e)
}

// ----------------------------------------------------------------------
// Limits
//
// The web bounds none of these inputs; its tool descriptions ask for a
// modest watchlist ("keep the list modest (≤ ~25)"). The desktop enforces
// that and the comparable sizes below, so one call cannot hold the history
// source, the broker or memory for long. A call over a limit is refused
// before anything is fetched, with a message that names the limit.
// ----------------------------------------------------------------------

/// Symbols in one `screen_instruments` call.
pub const MAX_SCREEN_SYMBOLS: usize = 25;
/// Timeframes in one `multi_timeframe_analysis` call.
pub const MAX_TIMEFRAMES: usize = 8;
/// `bars`, `lookback_bars` and `limit` values.
pub const MAX_BARS: i64 = 5000;
/// `lookback_days`, and the span of an explicit date range.
pub const MAX_DAYS: i64 = 3660;
/// Research calls running at once (process-wide); a call waits this long
/// for a slot before it is refused.
pub const RESEARCH_SLOTS: usize = 4;
pub const SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// Longest a research call may run in all (its `/api/v1` calls each have
/// their own 120 s limit).
pub const RESEARCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

fn limit_error(message: String) -> Value {
    error(
        message,
        &[
            ("error_type", json!("limit_exceeded")),
            ("retry_safe", json!(true)),
        ],
    )
}

/// The first limit the arguments exceed, as the tool's error output.
pub fn check_limits(tool: &str, a: &Map<String, Value>) -> Option<Value> {
    let n_list = |k: &str| a.get(k).and_then(Value::as_array).map_or(0, Vec::len);
    if tool == "screen_instruments" && n_list("symbols") > MAX_SCREEN_SYMBOLS {
        return Some(limit_error(format!(
            "screen_instruments accepts at most {} symbols per call. Split the watchlist into smaller groups.",
            MAX_SCREEN_SYMBOLS
        )));
    }
    if tool == "multi_timeframe_analysis" && n_list("intervals") > MAX_TIMEFRAMES {
        return Some(limit_error(format!(
            "multi_timeframe_analysis accepts at most {} intervals per call.",
            MAX_TIMEFRAMES
        )));
    }
    for k in ["bars", "lookback_bars", "limit"] {
        if i(a, k).is_some_and(|v| v.abs() > MAX_BARS) {
            return Some(limit_error(format!("'{}' can be at most {}.", k, MAX_BARS)));
        }
    }
    if i(a, "lookback_days").is_some_and(|v| v.abs() > MAX_DAYS) {
        return Some(limit_error(format!(
            "'lookback_days' can be at most {}.",
            MAX_DAYS
        )));
    }
    let date = |k: &str| s(a, k).and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok());
    if let (Some(start), Some(end)) = (date("start_date"), date("end_date")) {
        if (end - start).num_days().abs() > MAX_DAYS {
            return Some(limit_error(format!(
                "The date range can span at most {} days. Narrow start_date and end_date.",
                MAX_DAYS
            )));
        }
    }
    None
}

/// Run a research tool and turn a raised error into the web's `_fail`.
/// Bounded: argument limits first, then a process-wide slot, then an
/// overall deadline.
pub async fn run(ctx: &Arc<AppState>, tool: &str, a: &Map<String, Value>) -> Value {
    if let Some(refused) = check_limits(tool, a) {
        return refused;
    }
    let slots = ctx.mcp.research_slots();
    let Ok(Ok(_permit)) = tokio::time::timeout(SLOT_WAIT, slots.acquire_owned()).await else {
        return error(
            "OpenAlgo is busy with other analysis requests, so this one was not run. Try again in a moment.",
            &[("error_type", json!("busy")), ("retry_safe", json!(true))],
        );
    };
    match tokio::time::timeout(RESEARCH_DEADLINE, run_inner(ctx, tool, a)).await {
        Ok(v) => v,
        Err(_) => error(
            "This analysis took too long and was stopped. Use a shorter date range or fewer symbols.",
            &[("error_type", json!("timeout")), ("retry_safe", json!(true))],
        ),
    }
}

async fn run_inner(ctx: &Arc<AppState>, tool: &str, a: &Map<String, Value>) -> Value {
    let r = match tool {
        "get_historical_data" => historical(ctx, a).await,
        "calculate_indicator" => calculate_indicator(ctx, a).await,
        "get_trend_snapshot" => snapshot(ctx, a, Snap::Trend).await,
        "get_momentum_snapshot" => snapshot(ctx, a, Snap::Momentum).await,
        "get_volatility_snapshot" => snapshot(ctx, a, Snap::Volatility).await,
        "get_support_resistance" => snapshot(ctx, a, Snap::Levels).await,
        "detect_signals" => detect_signals(ctx, a).await,
        "screen_instruments" => screen(ctx, a).await,
        "multi_timeframe_analysis" => multi_timeframe(ctx, a).await,
        "correlation_beta" => correlation_beta(ctx, a).await,
        _ => Ok(error(format!("unknown tool '{}'", tool), &[])),
    };
    match r {
        Ok(v) => v,
        Err((action, e)) => fail(&action, &e.message, e.kind),
    }
}

async fn historical(ctx: &Arc<AppState>, a: &Map<String, Value>) -> ToolResult {
    let act = tag("getting historical data");
    let bars = i(a, "bars").unwrap_or(20);
    let mut w = window(a);
    w.lookback_bars = bars.max(252);
    let df = load_history(
        ctx,
        &s(a, "symbol").unwrap_or_default(),
        &s(a, "exchange").unwrap_or_default(),
        &s(a, "interval").unwrap_or_default(),
        &w,
    )
    .await
    .map_err(&act)?;
    let total = df.len() as i64;
    Ok(json!({
        "count": total,
        "returned": bars.min(total),
        "truncated": total > bars,
        "bars": bars,
        "data": records(&df.ts, df.intraday, &df.cols, bars),
    }))
}

fn stats(v: &[f64]) -> Value {
    let vals: Vec<f64> = v.iter().copied().filter(|x| !x.is_nan()).collect();
    if vals.is_empty() {
        return Value::Null;
    }
    let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    json!({
        "last": ta::py_round(vals[vals.len() - 1], 4),
        "min": ta::py_round(min, 4),
        "max": ta::py_round(max, 4),
        "mean": ta::py_round(mean, 4),
    })
}

async fn calculate_indicator(ctx: &Arc<AppState>, a: &Map<String, Value>) -> ToolResult {
    let act = tag("calculating indicator");
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let interval = s(a, "interval").unwrap_or_else(|| "D".into());
    let bars = i(a, "bars").unwrap_or(20);
    let w = window(a);
    let df = load_history(ctx, &symbol, &exchange, &interval, &w)
        .await
        .map_err(&act)?;
    let indicator = s(a, "indicator").unwrap_or_default();
    let name = match check_indicator(&indicator) {
        Ok(n) => n,
        Err(v) => return Ok(v),
    };
    let params = a
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let (cols, series) =
        resolve_inputs(&df, &name, a.get("inputs").and_then(Value::as_array)).map_err(&act)?;
    let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
    let result = ta::call(&name, &refs, &params).map_err(&act)?;
    let out_cols: Vec<(String, Vec<f64>)> = match &result {
        Out::One(v) => vec![("value".into(), v.clone())],
        Out::Many(v) => v
            .iter()
            .enumerate()
            .map(|(k, s)| (format!("out{}", k), s.clone()))
            .collect(),
    };
    let mut latest = Map::new();
    let mut summary = Map::new();
    for (c, v) in &out_cols {
        latest.insert(c.clone(), opt(ta::last(v)));
        summary.insert(c.clone(), stats(v));
    }
    let json_cols: Vec<(String, Vec<Value>)> = out_cols
        .iter()
        .map(|(c, v)| (c.clone(), f64_col(v)))
        .collect();
    let n = df.len() as i64;
    Ok(json!({
        "symbol": symbol.to_uppercase(),
        "exchange": exchange.to_uppercase(),
        "indicator": name,
        "inputs": cols,
        "params": params,
        "interval": interval,
        "source": w.source,
        "bars": n,
        "last_close": opt(ta::last(&df.close())),
        "latest_timestamp": df.iso(df.len() - 1),
        "latest": latest,
        "summary": summary,
        "returned_bars": bars.min(n),
        "data": records(&df.ts, df.intraday, &json_cols, bars),
    }))
}

enum Snap {
    Trend,
    Momentum,
    Volatility,
    Levels,
}

async fn snapshot(ctx: &Arc<AppState>, a: &Map<String, Value>, kind: Snap) -> ToolResult {
    let action = match kind {
        Snap::Trend => "getting trend snapshot",
        Snap::Momentum => "getting momentum snapshot",
        Snap::Volatility => "getting volatility snapshot",
        Snap::Levels => "getting support/resistance",
    };
    let act = tag(action);
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let interval = s(a, "interval").unwrap_or_else(|| "D".into());
    let df = load_history(ctx, &symbol, &exchange, &interval, &window(a))
        .await
        .map_err(&act)?;
    let (h, l, c) = (df.col("high"), df.col("low"), df.col("close"));
    let hlc = |name: &str, p: Map<String, Value>| -> Result<Out, PyErr> {
        let (h, l, c) = (h.clone()?, l.clone()?, c.clone()?);
        ta::call(name, &[&h, &l, &c], &p)
    };
    let one = |name: &str, p: Value| -> Result<Out, PyErr> {
        let c = c.clone()?;
        ta::call(name, &[&c], &kw(p))
    };
    let mut out = Map::new();
    out.insert("symbol".into(), json!(symbol.to_uppercase()));
    out.insert("exchange".into(), json!(exchange.to_uppercase()));
    out.insert("interval".into(), json!(interval));
    out.insert("from".into(), json!(df.iso(0)));
    out.insert("to".into(), json!(df.iso(df.len() - 1)));
    out.insert("bars_loaded".into(), json!(df.len()));
    match kind {
        Snap::Trend => {
            out.insert("last_close".into(), opt(ta::last(&df.close())));
            out.insert(
                "indicators".into(),
                bundle(vec![
                    ("sma_20", one("sma", json!({"period": 20}))),
                    ("sma_50", one("sma", json!({"period": 50}))),
                    ("sma_200", one("sma", json!({"period": 200}))),
                    ("ema_20", one("ema", json!({"period": 20}))),
                    ("ema_50", one("ema", json!({"period": 50}))),
                    ("supertrend", hlc("supertrend", no_params())),
                    ("adx_di", hlc("adx", kw(json!({"period": 14})))),
                    ("ichimoku", hlc("ichimoku", no_params())),
                ]),
            );
            out.insert(
                "legend".into(),
                json!({
                    "supertrend": "[supertrend_value, direction(+1 up / -1 down)]",
                    "adx_di": "[+DI, -DI, ADX]",
                    "ichimoku": "[tenkan, kijun, senkou_a, senkou_b, chikou]",
                }),
            );
        }
        Snap::Momentum => {
            out.insert("last_close".into(), opt(ta::last(&df.close())));
            out.insert(
                "indicators".into(),
                bundle(vec![
                    ("rsi_14", one("rsi", json!({"period": 14}))),
                    ("macd", one("macd", json!({}))),
                    ("stochastic", hlc("stochastic", no_params())),
                    ("cci_20", hlc("cci", kw(json!({"period": 20})))),
                    (
                        "williams_r_14",
                        hlc("williams_r", kw(json!({"period": 14}))),
                    ),
                ]),
            );
            out.insert(
                "legend".into(),
                json!({"macd": "[macd_line, signal_line, histogram]", "stochastic": "[%K, %D]"}),
            );
        }
        Snap::Volatility => {
            let donchian = || -> Result<Out, PyErr> {
                let (h, l) = (h.clone()?, l.clone()?);
                ta::call("donchian", &[&h, &l], &kw(json!({"period": 20})))
            };
            out.insert("last_close".into(), opt(ta::last(&df.close())));
            out.insert(
                "indicators".into(),
                bundle(vec![
                    ("atr_14", hlc("atr", kw(json!({"period": 14})))),
                    ("natr_14", hlc("natr", kw(json!({"period": 14})))),
                    (
                        "bbands",
                        one("bbands", json!({"period": 20, "std_dev": 2.0})),
                    ),
                    (
                        "bb_percent_b",
                        one("bbpercent", json!({"period": 20, "std_dev": 2.0})),
                    ),
                    (
                        "bb_width",
                        one("bbwidth", json!({"period": 20, "std_dev": 2.0})),
                    ),
                    ("keltner", hlc("keltner", no_params())),
                    ("donchian", donchian()),
                    ("historical_volatility", one("hv", json!({}))),
                ]),
            );
            out.insert(
                "legend".into(),
                json!({
                    "bbands": "[upper, middle, lower]",
                    "keltner": "[upper, middle, lower]",
                    "donchian": "[upper, middle, lower]",
                }),
            );
        }
        Snap::Levels => {
            let period = i(a, "period").unwrap_or(20);
            let hl = |name: &str, col: &Result<Vec<f64>, PyErr>| -> Result<Out, PyErr> {
                let v = col.clone()?;
                ta::call(name, &[&v], &kw(json!({"period": period})))
            };
            let donchian = || -> Result<Out, PyErr> {
                let (h, l) = (h.clone()?, l.clone()?);
                ta::call("donchian", &[&h, &l], &kw(json!({"period": period})))
            };
            out.insert("period".into(), json!(period));
            out.insert("last_close".into(), opt(ta::last(&df.close())));
            out.insert(
                "levels".into(),
                bundle(vec![
                    ("donchian", donchian()),
                    ("highest_high", hl("highest", &h)),
                    ("lowest_low", hl("lowest", &l)),
                    ("pivot_points", hlc("pivot_points", no_params())),
                ]),
            );
            out.insert(
                "legend".into(),
                json!({"donchian": "[upper, middle, lower]"}),
            );
        }
    }
    Ok(Value::Object(out))
}

fn one_series(o: Out) -> Vec<f64> {
    match o {
        Out::One(v) => v,
        Out::Many(mut v) => v.swap_remove(0),
    }
}

fn many(o: Out) -> Vec<Vec<f64>> {
    match o {
        Out::One(v) => vec![v],
        Out::Many(v) => v,
    }
}

/// `(x > lvl) & (x.shift(1) <= lvl)` style threshold crossings.
fn threshold(x: &[f64], now: impl Fn(f64) -> bool, before: impl Fn(f64) -> bool) -> Vec<bool> {
    (0..x.len())
        .map(|k| k > 0 && !x[k].is_nan() && !x[k - 1].is_nan() && now(x[k]) && before(x[k - 1]))
        .collect()
}

async fn detect_signals(ctx: &Arc<AppState>, a: &Map<String, Value>) -> ToolResult {
    let act = tag("detecting signals");
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let interval = s(a, "interval").unwrap_or_else(|| "D".into());
    let signal_type = s(a, "signal_type").unwrap_or_else(|| "ema_cross".into());
    let df = load_history(ctx, &symbol, &exchange, &interval, &window(a))
        .await
        .map_err(&act)?;
    let close = df.col("close").map_err(&act)?;
    let p = |k: &str, d: i64| json!({"period": i(a, k).unwrap_or(d)});
    let (bull, bear, current) = match signal_type.as_str() {
        "ema_cross" | "sma_cross" => {
            let ma = if signal_type == "ema_cross" {
                "ema"
            } else {
                "sma"
            };
            let fast = one_series(ta::call(ma, &[&close], &kw(p("fast", 20))).map_err(&act)?);
            let slow = one_series(ta::call(ma, &[&close], &kw(p("slow", 50))).map_err(&act)?);
            (
                ta::crossover(&fast, &slow),
                ta::crossunder(&fast, &slow),
                json!({"fast": opt(ta::last(&fast)), "slow": opt(ta::last(&slow))}),
            )
        }
        "macd_cross" => {
            let v = many(ta::call("macd", &[&close], &no_params()).map_err(&act)?);
            (
                ta::crossover(&v[0], &v[1]),
                ta::crossunder(&v[0], &v[1]),
                json!({"macd_line": opt(ta::last(&v[0])), "signal_line": opt(ta::last(&v[1]))}),
            )
        }
        "supertrend_flip" => {
            let (h, l) = (df.col("high").map_err(&act)?, df.col("low").map_err(&act)?);
            let v = many(ta::call("supertrend", &[&h, &l, &close], &no_params()).map_err(&act)?);
            let d = &v[1];
            (
                threshold(d, |x| x > 0.0, |x| x <= 0.0),
                threshold(d, |x| x < 0.0, |x| x >= 0.0),
                json!({"supertrend": opt(ta::last(&v[0])), "direction": opt(ta::last(d))}),
            )
        }
        "rsi_threshold" => {
            let upper = f(a, "upper").unwrap_or(70.0);
            let lower = f(a, "lower").unwrap_or(30.0);
            let r = one_series(ta::call("rsi", &[&close], &kw(p("period", 14))).map_err(&act)?);
            (
                threshold(&r, |x| x > lower, |x| x <= lower),
                threshold(&r, |x| x < upper, |x| x >= upper),
                json!({"rsi": opt(ta::last(&r))}),
            )
        }
        _ => return Ok(error(format!("unknown signal_type '{}'", signal_type), &[])),
    };
    let mut events: Vec<(String, &str)> = Vec::new();
    for (k, b) in bull.iter().enumerate() {
        if *b {
            events.push((df.iso(k), "bullish"));
        }
    }
    for (k, b) in bear.iter().enumerate() {
        if *b {
            events.push((df.iso(k), "bearish"));
        }
    }
    events.sort_by(|x, y| x.0.cmp(&y.0));
    let limit = i(a, "limit").unwrap_or(20);
    let start = tail_start(events.len(), limit);
    let list: Vec<Value> = events[start..]
        .iter()
        .map(|(t, sig)| json!({"timestamp": t, "signal": sig}))
        .collect();
    Ok(json!({
        "symbol": symbol.to_uppercase(),
        "exchange": exchange.to_uppercase(),
        "interval": interval,
        "signal_type": signal_type,
        "last_close": opt(ta::last(&close)),
        "current": current,
        "event_count": events.len(),
        "events": list,
    }))
}

async fn screen(ctx: &Arc<AppState>, a: &Map<String, Value>) -> ToolResult {
    let interval = s(a, "interval").unwrap_or_else(|| "D".into());
    let condition = s(a, "condition").unwrap_or_else(|| "rsi_below".into());
    let value = f(a, "value").unwrap_or(30.0);
    let period = i(a, "period").unwrap_or(14);
    let symbols = a
        .get("symbols")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let w = window(a);
    let mut results = Vec::new();
    for item in &symbols {
        let sym = item
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let exch = item
            .get("exchange")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let df = match load_history(ctx, sym, exch, &interval, &w).await {
            Ok(df) => df,
            Err(e) => {
                results.push(json!({"symbol": sym, "exchange": exch, "error": e.message}));
                continue;
            }
        };
        let evaluate = || -> Result<Option<(Value, bool)>, PyErr> {
            let close = df.col("close")?;
            Ok(match condition.as_str() {
                "rsi_below" | "rsi_above" => {
                    let m = ta::last(&one_series(ta::call(
                        "rsi",
                        &[&close],
                        &kw(json!({"period": period})),
                    )?));
                    let passed = m.is_some_and(|m| {
                        if condition == "rsi_below" {
                            m < value
                        } else {
                            m > value
                        }
                    });
                    Some((opt(m), passed))
                }
                "price_above_sma" | "price_below_sma" => {
                    let sma = ta::last(&one_series(ta::call(
                        "sma",
                        &[&close],
                        &kw(json!({"period": period})),
                    )?));
                    let c = ta::last(&close);
                    let passed = match (sma, c) {
                        (Some(sv), Some(cv)) => {
                            if condition == "price_above_sma" {
                                cv > sv
                            } else {
                                cv < sv
                            }
                        }
                        _ => false,
                    };
                    Some((opt(c), passed))
                }
                "supertrend_bullish" | "supertrend_bearish" => {
                    let (h, l) = (df.col("high")?, df.col("low")?);
                    let v = many(ta::call("supertrend", &[&h, &l, &close], &no_params())?);
                    let m = ta::last(&v[1]);
                    // As the web: a positive direction counts as bullish.
                    let passed = m.is_some_and(|m| {
                        if condition == "supertrend_bullish" {
                            m > 0.0
                        } else {
                            m < 0.0
                        }
                    });
                    Some((opt(m), passed))
                }
                _ => None,
            })
        };
        match evaluate() {
            Ok(Some((metric, passed))) => results.push(json!({
                "symbol": sym.to_uppercase(),
                "exchange": exch.to_uppercase(),
                "passed": passed,
                "metric": metric,
            })),
            Ok(None) => return Ok(error(format!("unknown condition '{}'", condition), &[])),
            Err(e) => results.push(json!({"symbol": sym, "exchange": exch, "error": e.message})),
        }
    }
    let matched = results
        .iter()
        .filter(|r| r.get("passed") == Some(&json!(true)))
        .count();
    Ok(json!({
        "condition": condition,
        "value": value,
        "period": period,
        "scanned": symbols.len(),
        "matched": matched,
        "results": results,
    }))
}

async fn multi_timeframe(ctx: &Arc<AppState>, a: &Map<String, Value>) -> ToolResult {
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let intervals: Vec<String> = a
        .get("intervals")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.iter()
                .map(|x| x.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_else(|| vec!["5m".into(), "15m".into(), "1h".into(), "D".into()]);
    let indicator = s(a, "indicator").unwrap_or_else(|| "rsi".into());
    let name = match check_indicator(&indicator) {
        Ok(n) => n,
        Err(v) => return Ok(v),
    };
    let params = a
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let w = window(a);
    let mut out = Map::new();
    for itv in &intervals {
        let r: Result<Value, PyErr> = async {
            let df = load_history(ctx, &symbol, &exchange, itv, &w).await?;
            let (_c, series) =
                resolve_inputs(&df, &name, a.get("inputs").and_then(Value::as_array))?;
            let refs: Vec<&[f64]> = series.iter().map(Vec::as_slice).collect();
            let res = ta::call(&name, &refs, &params)?;
            Ok(json!({
                "value": latest(&res),
                "last_close": opt(ta::last(&df.close())),
                "bars": df.len(),
            }))
        }
        .await;
        out.insert(
            itv.clone(),
            r.unwrap_or_else(|e| json!({"error": e.message})),
        );
    }
    Ok(json!({
        "symbol": symbol.to_uppercase(),
        "exchange": exchange.to_uppercase(),
        "indicator": name,
        "params": params,
        "timeframes": out,
    }))
}

fn pearson(x: &[f64], y: &[f64]) -> Value {
    let n = x.len() as f64;
    let (mx, my) = (x.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
    let num: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
    let syy: f64 = y.iter().map(|b| (b - my) * (b - my)).sum();
    let r = num / (sxx * syy).sqrt();
    if r.is_finite() {
        json!(ta::py_round(r, 4))
    } else {
        Value::Null
    }
}

async fn correlation_beta(ctx: &Arc<AppState>, a: &Map<String, Value>) -> ToolResult {
    let act = tag("calculating correlation/beta");
    let s1 = s(a, "symbol1").unwrap_or_default();
    let s2 = s(a, "symbol2").unwrap_or_default();
    let interval = s(a, "interval").unwrap_or_else(|| "D".into());
    let w = window(a);
    let d1 = load_history(
        ctx,
        &s1,
        &s(a, "exchange1").unwrap_or_default(),
        &interval,
        &w,
    )
    .await
    .map_err(&act)?;
    let d2 = load_history(
        ctx,
        &s2,
        &s(a, "exchange2").unwrap_or_default(),
        &interval,
        &w,
    )
    .await
    .map_err(&act)?;
    let (c1, c2) = (
        d1.col("close").map_err(&act)?,
        d2.col("close").map_err(&act)?,
    );
    // Align on common timestamps and drop rows with a missing close.
    let mut xa = Vec::new();
    let mut xb = Vec::new();
    let mut j = 0;
    for (k, t) in d1.ts.iter().enumerate() {
        while j < d2.ts.len() && d2.ts[j] < *t {
            j += 1;
        }
        if j < d2.ts.len() && d2.ts[j] == *t && !c1[k].is_nan() && !c2[j].is_nan() {
            xa.push(c1[k]);
            xb.push(c2[j]);
        }
    }
    if xa.len() < 2 {
        return Ok(error("insufficient overlapping bars between symbols", &[]));
    }
    let p = i(a, "period").unwrap_or(20).min(xa.len() as i64);
    let pp = kw(json!({"period": p}));
    let mut metrics = match bundle(vec![
        (
            "correlation_rolling",
            ta::call("correlation", &[&xa, &xb], &pp),
        ),
        ("beta_rolling", ta::call("beta", &[&xa, &xb], &pp)),
        ("lrslope_symbol1", ta::call("lrslope", &[&xa], &pp)),
    ]) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    metrics.insert("pearson_full_sample".into(), pearson(&xa, &xb));
    Ok(json!({
        "symbol1": s1.to_uppercase(),
        "symbol2": s2.to_uppercase(),
        "interval": interval,
        "period": p,
        "overlapping_bars": xa.len(),
        "metrics": metrics,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sorts_dedupes_and_formats_like_pandas() {
        let data = vec![
            json!({"timestamp": 1759290300, "close": 2.5, "volume": 20}),
            json!({"timestamp": 1759290000, "close": 100.123456789123, "volume": 10}),
            json!({"timestamp": 1759290000, "close": 1.0, "volume": 1}),
        ];
        let f = frame_from(&data, true);
        assert_eq!(f.ts, vec![1759290000, 1759290300]);
        assert_eq!(f.iso(0), "2025-10-01T09:10:00+05:30");
        let r = records(&f.ts, f.intraday, &f.cols, 0);
        assert_eq!(r[0]["timestamp"], "2025-10-01T03:40:00.000Z");
        assert_eq!(r[0]["close"], json!(100.1234567891));
        assert_eq!(r[0]["volume"], json!(10));
        let d = frame_from(&data, false);
        assert_eq!(d.iso(0), "2025-10-01T03:40:00");
        assert_eq!(
            records(&d.ts, false, &d.cols, 1)[0]["timestamp"],
            "2025-10-01T03:45:00.000"
        );
    }

    #[test]
    fn python_tail_slices() {
        assert_eq!(tail_start(5, 2), 3);
        assert_eq!(tail_start(5, 0), 0);
        assert_eq!(tail_start(5, 9), 0);
        assert_eq!(tail_start(5, -2), 2);
    }
}
