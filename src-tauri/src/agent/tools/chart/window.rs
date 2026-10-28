//! The chart's own candles (the web's `ChartToolkit._window` and `_fetch`).
//!
//! The instrument, exchange and interval come from the chart context,
//! never from an argument: that is what keeps a poisoned string away from
//! the history service, and what makes every tool answer about the chart in
//! front of the trader. The fetch is cached for the run by its clamped bar
//! count, so the geometry and the momentum block read identical bars.

use super::ChartRun;
use crate::agent::chart_contract::ChartView;
use crate::agent::chart_geometry::Bars;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::tools::market::candles::{candle_columns, candle_frame, ist_label, CandleFrame};
use crate::agent::tools::market::normalise::{
    lookback_range, normalise_interval, normalise_pair, today,
};
use crate::agent::tools::market::{data_of, master};
use crate::agent::tools::numbers::as_number;
use crate::agent::tools::values::quote;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Bars fetched when the model names no lookback: over a year of daily
/// structure, past the warm-up of every momentum indicator.
pub(super) const DEFAULT_LOOKBACK: u32 = 300;

/// Floor on the lookback: below it the pivot detector has nothing to work
/// with.
pub(super) const MIN_LOOKBACK: u32 = 60;

/// Ceiling on the lookback: past it the fetch is slow and the extra bars
/// are off screen anyway.
pub(super) const MAX_LOOKBACK: u32 = 1500;

/// One candle fetch, in the shape the geometry reads and the shape the
/// indicators read.
pub(super) struct Fetched {
    pub bars: Bars,
    pub frame: CandleFrame,
}

/// The accounting every windowed result repeats, in the web's order.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Meta {
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub timezone: &'static str,
    pub bars_fetched: usize,
    pub bars_analysed: usize,
    pub window_from: String,
    pub window_to: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// The fetch, clipped to the trader's viewport.
pub(super) struct Window {
    pub fetched: Arc<Fetched>,
    pub lo: usize,
    pub hi: usize,
    pub meta: Meta,
}

impl Window {
    pub fn bars(&self) -> &Bars {
        &self.fetched.bars
    }
}

/// The model's bar count clamped (0 or anything not a number is the
/// default).
pub(super) fn clamp_lookback(raw: Option<&Value>) -> u32 {
    let wanted = raw
        .filter(|v| v.is_number())
        .and_then(as_number)
        .map_or(0.0, f64::trunc);
    if wanted <= 0.0 {
        DEFAULT_LOOKBACK
    } else {
        wanted.clamp(f64::from(MIN_LOOKBACK), f64::from(MAX_LOOKBACK)) as u32
    }
}

/// The chart's candles for this lookback, clipped to the viewport.
pub(super) async fn window(
    run: &ChartRun,
    ctx: &ToolCtx,
    view: &ChartView,
    lookback: Option<&Value>,
) -> Result<Window, ToolOutcome> {
    let (symbol, exchange, interval, mut notices) = instrument(run, ctx, view).await?;
    let wanted = clamp_lookback(lookback);
    let fetched = {
        let mut cache = run.windows.lock().await;
        match cache.get(&wanted) {
            Some(hit) => hit.clone(),
            None => {
                let fresh = Arc::new(fetch(ctx, &symbol, &exchange, &interval, wanted).await?);
                cache.insert(wanted, fresh.clone());
                fresh
            }
        }
    };
    let (lo, hi) = fetched.bars.window(view.visible_from, view.visible_to);
    let mut unique: Vec<String> = Vec::new();
    for n in notices.drain(..) {
        if !unique.contains(&n) {
            unique.push(n);
        }
    }
    let times = &fetched.bars.times;
    let meta = Meta {
        symbol,
        exchange,
        interval,
        timezone: "Asia/Kolkata",
        bars_fetched: fetched.bars.len(),
        bars_analysed: hi - lo + 1,
        window_from: ist_label(&json!(times[lo])),
        window_to: ist_label(&json!(times[hi])),
        notices: unique,
    };
    Ok(Window {
        fetched,
        lo,
        hi,
        meta,
    })
}

/// The chart's symbol, exchange and interval as the history service takes
/// them, with any correction as a notice. A refusal names the chart, not an
/// argument the model never passed.
async fn instrument(
    run: &ChartRun,
    ctx: &ToolCtx,
    view: &ChartView,
) -> Result<(String, String, String, Vec<String>), ToolOutcome> {
    let unusable = |refusal: ToolOutcome| {
        ToolOutcome::Retry(format!(
            "The chart context is unusable: {}. It reported symbol {} on exchange {} at interval {}. Tell the operator their chart is on an instrument or a resolution this broker does not serve history for; do not retry.",
            refusal.text(),
            quote(&view.symbol),
            quote(&view.exchange),
            quote(&view.interval)
        ))
    };
    let listed = master(&ctx.app);
    let (symbol, exchange, mut notices) = normalise_pair(
        &listed,
        Some(&Value::from(view.symbol.as_str())),
        Some(&Value::from(view.exchange.as_str())),
    )
    .map_err(unusable)?;
    let accepted = run.intervals.accepted(ctx).await;
    let (interval, correction) = normalise_interval(
        Some(&Value::from(view.interval.as_str())),
        "api",
        accepted.as_deref(),
    )
    .map_err(unusable)?;
    notices.extend(correction);
    Ok((symbol, exchange, interval, notices))
}

/// One candle window from the history service, newest `bars` kept.
async fn fetch(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
    interval: &str,
    bars: u32,
) -> Result<Fetched, ToolOutcome> {
    let (start, end) = lookback_range(interval, bars, today(&ctx.app));
    let mut request = Map::new();
    request.insert("symbol".into(), json!(symbol));
    request.insert("exchange".into(), json!(exchange));
    request.insert("interval".into(), json!(interval));
    request.insert("start_date".into(), json!(start));
    request.insert("end_date".into(), json!(end));
    let response = ctx
        .service_read("history_service.get_history", "history", request)
        .await?;
    let records: Vec<Map<String, Value>> = match data_of(response) {
        Value::Array(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let columns = records.first().map(candle_columns).unwrap_or_default();
    let frame = newest(candle_frame(&records, &columns), bars as usize);
    let window = Bars::from_frame(&frame);
    if window.len() < 2 {
        return Err(ToolOutcome::Retry(format!(
            "No usable candles came back for {} on {} at {} between {} and {}, so there is nothing to measure. Tell the operator the broker served no history for the chart's instrument at this resolution, and do not read levels off the chart instead.",
            symbol, exchange, interval, start, end
        )));
    }
    Ok(Fetched {
        bars: window,
        frame,
    })
}

/// The newest `keep` rows of a frame (`frame.iloc[-bars:]`).
fn newest(mut frame: CandleFrame, keep: usize) -> CandleFrame {
    let drop = frame.len().saturating_sub(keep);
    if drop > 0 {
        frame.time.drain(..drop);
        for (_, column) in frame.columns.iter_mut() {
            column.drain(..drop);
        }
    }
    frame
}
