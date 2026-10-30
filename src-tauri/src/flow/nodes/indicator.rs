//! The Indicator node (the web's `execute_indicator`): any `openalgo.ta`
//! indicator over a symbol's history, or nested on another node's series
//! (`{{rsi1.series}}`, `{{h.data}}`, plain numbers) when `sourceSeries` is
//! set. The numbers come from the indicators lane's port of
//! `services/indicator_service.py`; this node reads its fields, the params
//! JSON and the series, fetches history through the shared cache and writes
//! the web's log lines.
//!
//! Two web defects are fixed here (parity N-indicator-03, N-indicator-11 /
//! IND-043): a whitespace-only `sourceSeries` is absent, as the validator
//! treats it, rather than a nested series that cannot parse; and every
//! failure of the computation fails only this node with `Indicator error:
//! {e}`, where the web let anything but a `ValueError` stop the whole run.

use super::history::{fetch_cached, history_error, records_of, today_ist};
use super::pyjson;
use crate::flow::contract::node::{FlowNode, NodeAbort, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::indicator_service::{
    compute_indicator, history_window, source_series_numbers, IndicatorOutput, IndicatorParams,
    IndicatorRequest, IndicatorSource,
};
use async_trait::async_trait;
use serde_json::{Map, Value};

/// The Indicator node.
pub struct IndicatorNode;

/// The node's numeric settings (`int()` with the web's defaults).
struct Bars {
    lookback: i64,
    tail: i64,
    offset: i64,
}

#[async_trait]
impl FlowNode for IndicatorNode {
    fn node_type(&self) -> &'static str {
        "indicator"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let name = ctx.get_str(data, "indicatorName", "sma");
        // Read in the web's order; a list or dict (or `inf`) where a whole
        // number belongs ends the run as the web's int() does (decision 24).
        let bars = match read_bars(ctx, data) {
            Ok(b) => b,
            Err(abort) => return abort.into(),
        };
        let source_field = ctx.get_str(data, "sourceField", "");
        let params = match read_params(ctx, data) {
            Ok(p) => p,
            Err(message) => return NodeOutcome::error(message),
        };
        match data.get("sourceSeries").filter(|v| is_nested(v)) {
            Some(raw) => nested(ctx, data, raw, &name, &params, &bars, &source_field),
            None => from_history(ctx, data, &name, &params, &bars).await,
        }
    }
}

fn read_bars(ctx: &NodeCtx, data: &Map<String, Value>) -> Result<Bars, NodeAbort> {
    Ok(Bars {
        lookback: ctx.try_get_int(data, "lookbackBars", 100)?,
        tail: ctx.try_get_int(data, "tailBars", 5)?,
        offset: ctx.try_get_int(data, "offsetBars", 0)?,
    })
}

/// Nested mode is chosen by a `sourceSeries` that is set: not null, not
/// empty and, unlike the web, not only whitespace (N-indicator-03).
fn is_nested(raw: &Value) -> bool {
    match raw {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

/// The node's `params`: the JSON text (interpolated as a whole), or the
/// legacy object form, normalised with a warning.
fn read_params(ctx: &NodeCtx, data: &Map<String, Value>) -> Result<IndicatorParams, String> {
    if let Some(Value::Object(map)) = data.get("params") {
        ctx.log(
            "Indicator params were stored as an object; normalising to the JSON string form. Re-save the node to store it correctly.",
            WARNING,
        );
        return Ok(IndicatorParams::from_map(map));
    }
    IndicatorParams::parse(&ctx.get_str(data, "params", "{}"))
}

/// The series a nested indicator runs on: the stored list itself for a
/// field that is one `{{...}}`, else the text read as Python reads JSON.
/// `Err` is the node's message after its log line.
fn nested_values(ctx: &NodeCtx, raw: &Value, source_field: &str) -> Result<Vec<f64>, String> {
    let resolved = match raw {
        Value::String(_) => ctx.vars.resolve_raw(raw),
        other => other.clone(),
    };
    let list = match &resolved {
        Value::String(text) => match pyjson::loads(text) {
            Ok(v) => v,
            Err(_) => {
                ctx.error(format!(
                    "sourceSeries did not resolve to an array: {}",
                    pyfmt::repr(&resolved)
                ));
                return Err("sourceSeries did not resolve to a JSON array".into());
            }
        },
        other => other.clone(),
    };
    // JSON text holding a string (`"abc"`) is not an array; handed on, it
    // would be read as JSON a second time.
    if list.is_string() {
        ctx.error(format!(
            "sourceSeries must resolve to an array, got: {}",
            pyfmt::repr(&list)
        ));
        return Err("sourceSeries must resolve to an array".into());
    }
    source_series_numbers(&list, source_field).map_err(|e| {
        ctx.error(e.log);
        e.message
    })
}

fn nested(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    raw: &Value,
    name: &str,
    params: &IndicatorParams,
    bars: &Bars,
    source_field: &str,
) -> NodeOutcome {
    let values = match nested_values(ctx, raw, source_field) {
        Ok(v) => v,
        Err(message) => return NodeOutcome::error(message),
    };
    ctx.info(format!(
        "Computing nested {} over {} upstream values",
        name,
        values.len()
    ));
    let request = IndicatorRequest {
        indicator_name: name,
        params,
        source: IndicatorSource::Nested(&values),
        tail_bars: bars.tail,
        offset_bars: bars.offset,
    };
    finish(
        ctx,
        data,
        compute_indicator(&request),
        &format!("{} (nested)", name),
    )
}

async fn from_history(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    name: &str,
    params: &IndicatorParams,
    bars: &Bars,
) -> NodeOutcome {
    // Interpolated once (N-dataCommon-14).
    let symbol = ctx.get_str(data, "symbol", "");
    let exchange = ctx.get_str(data, "exchange", "NSE");
    let interval = ctx.get_str(data, "interval", "D");
    let source = ctx.get_str(data, "source", "api");
    if symbol.is_empty() {
        return NodeOutcome::error("Symbol is required");
    }
    let window = history_window(bars.lookback, None, &interval, today_ist(ctx));
    ctx.info(format!(
        "Computing {} for {} ({}, {} bars)",
        name, symbol, interval, bars.lookback
    ));
    let history = fetch_cached(ctx, &symbol, &exchange, &interval, window, &source).await;
    if let Some(message) = history_error(&history, &symbol, &interval, &source) {
        ctx.error(message.clone());
        return NodeOutcome::error(message);
    }
    let records = records_of(&history);
    let request = IndicatorRequest {
        indicator_name: name,
        params,
        source: IndicatorSource::Candles {
            records: &records,
            lookback_bars: bars.lookback,
        },
        tail_bars: bars.tail,
        offset_bars: bars.offset,
    };
    finish(ctx, data, compute_indicator(&request), name)
}

/// Log the latest value, store and return the result; any failure of the
/// computation is this node's error (IND-043).
fn finish(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    computed: Result<IndicatorOutput, crate::flow::indicator_service::IndicatorError>,
    label: &str,
) -> NodeOutcome {
    let output = match computed {
        Ok(output) => output.to_value(),
        Err(e) => {
            ctx.error(format!("Indicator error: {}", e));
            return NodeOutcome::error(e.message);
        }
    };
    let latest = output.get("latest").cloned().unwrap_or(Value::Null);
    ctx.info(format!("{} latest: {}", label, pyfmt::str(&latest)));
    ctx.store_output(data, output.clone());
    NodeOutcome::result(output)
}
