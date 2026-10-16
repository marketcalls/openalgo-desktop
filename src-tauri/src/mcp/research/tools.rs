//! The research tools themselves (`mcpserver.py` "RESEARCH TOOLS").

use super::args::{
    bundle, check_indicator, f, i, kw, latest, lookback, no_params, resolve_inputs, s, tag,
    ToolResult,
};
use super::history::{
    f64_col, load_history, opt, plan, records, resolve, tail_start, Frame, Interval,
};
use super::limits::{admit, within_deadline};
use crate::mcp::envelope::{error, fail};
use crate::mcp::ta::{self, Out, PyErr};
use crate::state::AppState;
use chrono::NaiveDate;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Run a research tool and turn a raised error into the web's `_fail`.
/// Admitted first (see [`admit`]), then bounded by an overall deadline.
pub async fn run(ctx: &Arc<AppState>, tool: &str, a: &Map<String, Value>) -> Value {
    let admission = match admit(ctx, tool, a).await {
        Ok(adm) => adm,
        Err(refused) => return refused,
    };
    let (a, today) = (&admission.args, admission.today);
    within_deadline(
        run_inner(ctx, tool, a, today),
        "This analysis took too long and was stopped. Use a shorter date range or fewer symbols.",
    )
    .await
}

async fn run_inner(
    ctx: &Arc<AppState>,
    tool: &str,
    a: &Map<String, Value>,
    today: NaiveDate,
) -> Value {
    let r = match tool {
        "get_historical_data" => historical(ctx, a, today).await,
        "calculate_indicator" => calculate_indicator(ctx, a, today).await,
        "get_trend_snapshot" => snapshot(ctx, a, Snap::Trend, today).await,
        "get_momentum_snapshot" => snapshot(ctx, a, Snap::Momentum, today).await,
        "get_volatility_snapshot" => snapshot(ctx, a, Snap::Volatility, today).await,
        "get_support_resistance" => snapshot(ctx, a, Snap::Levels, today).await,
        "detect_signals" => detect_signals(ctx, a, today).await,
        "screen_instruments" => screen(ctx, a, today).await,
        "multi_timeframe_analysis" => multi_timeframe(ctx, a, today).await,
        "correlation_beta" => correlation_beta(ctx, a, today).await,
        _ => Ok(error(format!("unknown tool '{}'", tool), &[])),
    };
    match r {
        Ok(v) => v,
        Err((action, e)) => fail(&action, &e.message, e.kind),
    }
}

async fn historical(ctx: &Arc<AppState>, a: &Map<String, Value>, today: NaiveDate) -> ToolResult {
    let act = tag("getting historical data");
    let bars = i(a, "bars").unwrap_or(20);
    let (itv, w) = plan(a, "", bars.max(252), today).map_err(&act)?;
    let df = load_history(
        ctx,
        &s(a, "symbol").unwrap_or_default(),
        &s(a, "exchange").unwrap_or_default(),
        &itv,
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

async fn calculate_indicator(
    ctx: &Arc<AppState>,
    a: &Map<String, Value>,
    today: NaiveDate,
) -> ToolResult {
    let act = tag("calculating indicator");
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let bars = i(a, "bars").unwrap_or(20);
    let (itv, w) = plan(a, "D", lookback(a), today).map_err(&act)?;
    let interval = itv.raw.clone();
    let df = load_history(ctx, &symbol, &exchange, &itv, &w)
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

async fn snapshot(
    ctx: &Arc<AppState>,
    a: &Map<String, Value>,
    kind: Snap,
    today: NaiveDate,
) -> ToolResult {
    let action = match kind {
        Snap::Trend => "getting trend snapshot",
        Snap::Momentum => "getting momentum snapshot",
        Snap::Volatility => "getting volatility snapshot",
        Snap::Levels => "getting support/resistance",
    };
    let act = tag(action);
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let (itv, w) = plan(a, "D", lookback(a), today).map_err(&act)?;
    let interval = itv.raw.clone();
    let df = load_history(ctx, &symbol, &exchange, &itv, &w)
        .await
        .map_err(&act)?;
    let cols = Cols {
        h: df.col("high"),
        l: df.col("low"),
        c: df.col("close"),
    };
    let mut out = Map::new();
    out.insert("symbol".into(), json!(symbol.to_uppercase()));
    out.insert("exchange".into(), json!(exchange.to_uppercase()));
    out.insert("interval".into(), json!(interval));
    out.insert("from".into(), json!(df.iso(0)));
    out.insert("to".into(), json!(df.iso(df.len() - 1)));
    out.insert("bars_loaded".into(), json!(df.len()));
    let last_close = opt(ta::last(&df.close()));
    match kind {
        Snap::Trend => trend_snapshot(&cols, &mut out, last_close),
        Snap::Momentum => momentum_snapshot(&cols, &mut out, last_close),
        Snap::Volatility => volatility_snapshot(&cols, &mut out, last_close),
        Snap::Levels => {
            let period = i(a, "period").unwrap_or(20);
            levels_snapshot(&cols, &mut out, last_close, period)
        }
    }
    Ok(Value::Object(out))
}

/// The price columns a snapshot computes from (each may be missing).
struct Cols {
    h: Result<Vec<f64>, PyErr>,
    l: Result<Vec<f64>, PyErr>,
    c: Result<Vec<f64>, PyErr>,
}

impl Cols {
    /// An indicator of high, low and close.
    fn hlc(&self, name: &str, p: Map<String, Value>) -> Result<Out, PyErr> {
        let (h, l, c) = (self.h.clone()?, self.l.clone()?, self.c.clone()?);
        ta::call(name, &[&h, &l, &c], &p)
    }

    /// An indicator of the close.
    fn one(&self, name: &str, p: Value) -> Result<Out, PyErr> {
        let c = self.c.clone()?;
        ta::call(name, &[&c], &kw(p))
    }

    /// The Donchian channel over `period` bars.
    fn donchian(&self, period: i64) -> Result<Out, PyErr> {
        let (h, l) = (self.h.clone()?, self.l.clone()?);
        ta::call("donchian", &[&h, &l], &kw(json!({"period": period})))
    }
}

/// `get_trend_snapshot`: moving averages, Supertrend, ADX/DI, Ichimoku.
fn trend_snapshot(cols: &Cols, out: &mut Map<String, Value>, last_close: Value) {
    out.insert("last_close".into(), last_close);
    out.insert(
        "indicators".into(),
        bundle(vec![
            ("sma_20", cols.one("sma", json!({"period": 20}))),
            ("sma_50", cols.one("sma", json!({"period": 50}))),
            ("sma_200", cols.one("sma", json!({"period": 200}))),
            ("ema_20", cols.one("ema", json!({"period": 20}))),
            ("ema_50", cols.one("ema", json!({"period": 50}))),
            ("supertrend", cols.hlc("supertrend", no_params())),
            ("adx_di", cols.hlc("adx", kw(json!({"period": 14})))),
            ("ichimoku", cols.hlc("ichimoku", no_params())),
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

/// `get_momentum_snapshot`: RSI, MACD, stochastic, CCI, Williams %R.
fn momentum_snapshot(cols: &Cols, out: &mut Map<String, Value>, last_close: Value) {
    out.insert("last_close".into(), last_close);
    out.insert(
        "indicators".into(),
        bundle(vec![
            ("rsi_14", cols.one("rsi", json!({"period": 14}))),
            ("macd", cols.one("macd", json!({}))),
            ("stochastic", cols.hlc("stochastic", no_params())),
            ("cci_20", cols.hlc("cci", kw(json!({"period": 20})))),
            (
                "williams_r_14",
                cols.hlc("williams_r", kw(json!({"period": 14}))),
            ),
        ]),
    );
    out.insert(
        "legend".into(),
        json!({"macd": "[macd_line, signal_line, histogram]", "stochastic": "[%K, %D]"}),
    );
}

/// `get_volatility_snapshot`: ATR, NATR, Bollinger, Keltner, Donchian, HV.
fn volatility_snapshot(cols: &Cols, out: &mut Map<String, Value>, last_close: Value) {
    out.insert("last_close".into(), last_close);
    out.insert(
        "indicators".into(),
        bundle(vec![
            ("atr_14", cols.hlc("atr", kw(json!({"period": 14})))),
            ("natr_14", cols.hlc("natr", kw(json!({"period": 14})))),
            (
                "bbands",
                cols.one("bbands", json!({"period": 20, "std_dev": 2.0})),
            ),
            (
                "bb_percent_b",
                cols.one("bbpercent", json!({"period": 20, "std_dev": 2.0})),
            ),
            (
                "bb_width",
                cols.one("bbwidth", json!({"period": 20, "std_dev": 2.0})),
            ),
            ("keltner", cols.hlc("keltner", no_params())),
            ("donchian", cols.donchian(20)),
            ("historical_volatility", cols.one("hv", json!({}))),
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

/// `get_support_resistance`: Donchian, highest high, lowest low, pivots.
fn levels_snapshot(cols: &Cols, out: &mut Map<String, Value>, last_close: Value, period: i64) {
    let hl = |name: &str, col: &Result<Vec<f64>, PyErr>| -> Result<Out, PyErr> {
        let v = col.clone()?;
        ta::call(name, &[&v], &kw(json!({"period": period})))
    };
    out.insert("period".into(), json!(period));
    out.insert("last_close".into(), last_close);
    out.insert(
        "levels".into(),
        bundle(vec![
            ("donchian", cols.donchian(period)),
            ("highest_high", hl("highest", &cols.h)),
            ("lowest_low", hl("lowest", &cols.l)),
            ("pivot_points", cols.hlc("pivot_points", no_params())),
        ]),
    );
    out.insert(
        "legend".into(),
        json!({"donchian": "[upper, middle, lower]"}),
    );
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

async fn detect_signals(
    ctx: &Arc<AppState>,
    a: &Map<String, Value>,
    today: NaiveDate,
) -> ToolResult {
    let act = tag("detecting signals");
    let symbol = s(a, "symbol").unwrap_or_default();
    let exchange = s(a, "exchange").unwrap_or_default();
    let signal_type = s(a, "signal_type").unwrap_or_else(|| "ema_cross".into());
    let (itv, w) = plan(a, "D", lookback(a), today).map_err(&act)?;
    let interval = itv.raw.clone();
    let df = load_history(ctx, &symbol, &exchange, &itv, &w)
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

/// One instrument against a screen condition: its metric and whether it
/// passed, or `None` for a condition the screen does not know.
fn screen_check(
    df: &Frame,
    condition: &str,
    value: f64,
    period: i64,
) -> Result<Option<(Value, bool)>, PyErr> {
    let close = df.col("close")?;
    Ok(match condition {
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
}

async fn screen(ctx: &Arc<AppState>, a: &Map<String, Value>, today: NaiveDate) -> ToolResult {
    let condition = s(a, "condition").unwrap_or_else(|| "rsi_below".into());
    let value = f(a, "value").unwrap_or(30.0);
    let period = i(a, "period").unwrap_or(14);
    let symbols = a
        .get("symbols")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (itv, w) = match plan(a, "D", lookback(a), today) {
        Ok(p) => p,
        Err(e) => return Err(("screening instruments".to_string(), e)),
    };
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
        let df = match load_history(ctx, sym, exch, &itv, &w).await {
            Ok(df) => df,
            Err(e) => {
                results.push(json!({"symbol": sym, "exchange": exch, "error": e.message}));
                continue;
            }
        };
        match screen_check(&df, &condition, value, period) {
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

async fn multi_timeframe(
    ctx: &Arc<AppState>,
    a: &Map<String, Value>,
    today: NaiveDate,
) -> ToolResult {
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
    let lookback_bars = lookback(a);
    let mut out = Map::new();
    for itv in &intervals {
        let r: Result<Value, PyErr> = async {
            let parsed = Interval::parse(itv)
                .ok_or_else(|| PyErr::value(format!("unsupported interval '{}'", itv)))?;
            let w = resolve(a, &parsed, lookback_bars, today).map_err(PyErr::value)?;
            let df = load_history(ctx, &symbol, &exchange, &parsed, &w).await?;
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

async fn correlation_beta(
    ctx: &Arc<AppState>,
    a: &Map<String, Value>,
    today: NaiveDate,
) -> ToolResult {
    let act = tag("calculating correlation/beta");
    let s1 = s(a, "symbol1").unwrap_or_default();
    let s2 = s(a, "symbol2").unwrap_or_default();
    let (itv, w) = plan(a, "D", lookback(a), today).map_err(&act)?;
    let interval = itv.raw.clone();
    let d1 = load_history(ctx, &s1, &s(a, "exchange1").unwrap_or_default(), &itv, &w)
        .await
        .map_err(&act)?;
    let d2 = load_history(ctx, &s2, &s(a, "exchange2").unwrap_or_default(), &itv, &w)
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
