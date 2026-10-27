//! The combined premium series (the web's `_rolling_atm`, `_fixed_legs` and
//! `_deliver_premium` in `tools/option_viz.py`).
//!
//! The series is one number per bar, sent as the frame's bars with
//! open = high = low = close, so the chart draws a line and a study reads
//! the combination itself. The legs are inner-joined on the timestamp and
//! the drop is reported: a leg that did not print in a window would
//! otherwise shorten the series silently, and a missing leg counted as zero
//! draws a collapse that never happened.

use super::{MAX_PREMIUM_POINTS, PREMIUM_VIZ};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::contract::viz::no_sink_message;
use crate::agent::tools::answer::{label, tool_answer};
use crate::agent::tools::market::candles::{candle_columns, chart_bars};
use crate::agent::tools::market::master;
use crate::agent::tools::numbers::{as_number, format_number, format_price, price, py_round};
use crate::agent::tools::options::normalise_symbol;
use crate::agent::tools::options_common::{
    leg_label, leg_side_and_lots, leg_symbol, master_lot_size, normalise_leg_expiry,
    resolve_expiry, resolve_leg_exchange, resolve_underlying_exchange, segment_of,
    signed_multiplier,
};
use crate::agent::tools::values::truthy_text;
use crate::agent::tools::viz::{arg_or, history_records, tools_service, TIMEZONE};
use crate::analytics::series::{cap_last_n_dates, trading_window};
use crate::services::history_tools_service;
use crate::services::options_service::today_ist;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const TOOL: &str = "plot_combined_premium";

/// Legs named in a chart heading before it stops being a heading.
const MAX_HEADING_LEGS: usize = 3;

/// What the caller settled before choosing a series.
pub struct Request {
    pub interval: String,
    pub sessions: i64,
    pub overlays: Vec<Value>,
    pub notices: Vec<String>,
}

/// How a premium chart left.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivered {
    Drawn,
    /// The run has no sink.
    NoSink,
    /// No point of the series was a finite number (desktop, D-030: the web
    /// reported this as a delivery failure).
    NoPoints,
}

/// One premium chart, before it leaves.
pub struct PremiumChart<'a> {
    /// Bar times (epoch seconds), parallel to `combined`.
    pub times: Vec<i64>,
    pub combined: Vec<Option<f64>>,
    pub title: String,
    pub subtitle: String,
    /// What the values are, for the reader.
    pub axis: &'a str,
    pub interval: &'a str,
    pub indicators: &'a [Value],
    pub source: &'a str,
}

/// The `candles` spec of a premium chart: the newest
/// [`MAX_PREMIUM_POINTS`] points (a notice when older ones were cut), each a
/// flat bar; `None` when no point is a finite number.
pub fn premium_spec(
    chart: &PremiumChart<'_>,
    notices: &mut Vec<String>,
) -> Option<Map<String, Value>> {
    let total = chart.times.len();
    let keep = total.min(MAX_PREMIUM_POINTS);
    if total > keep {
        notices.push(format!(
            "{} of {} points were older than the {} the chart carries, so it starts later than the range asked for.",
            total - keep,
            total,
            MAX_PREMIUM_POINTS
        ));
    }
    let bars: Vec<Value> = chart.times[total - keep..]
        .iter()
        .zip(&chart.combined[chart.combined.len().saturating_sub(keep)..])
        .filter_map(|(time, value)| {
            let v = value.filter(|v| v.is_finite())?;
            Some(json!({"time": time, "open": v, "high": v, "low": v, "close": v}))
        })
        .collect();
    if bars.is_empty() {
        return None;
    }
    let mut spec = Map::new();
    spec.insert("symbol".into(), json!(chart.title));
    spec.insert("exchange".into(), json!(""));
    spec.insert("interval".into(), json!(chart.interval));
    spec.insert("chart_type".into(), json!("line"));
    spec.insert("timezone".into(), json!(TIMEZONE));
    spec.insert("bar_count".into(), json!(bars.len()));
    spec.insert("bars".into(), Value::Array(bars));
    spec.insert("indicators".into(), json!(chart.indicators));
    spec.insert("value_label".into(), json!(chart.axis));
    spec.insert("subtitle".into(), json!(chart.subtitle));
    if !notices.is_empty() {
        spec.insert("notices".into(), json!(notices));
    }
    Some(spec)
}

/// Leave the premium chart on the run's sink (no `json_safe`, as the web).
pub fn deliver_premium(
    ctx: &ToolCtx,
    chart: &PremiumChart<'_>,
    notices: &mut Vec<String>,
) -> Delivered {
    let Some(spec) = premium_spec(chart, notices) else {
        return Delivered::NoPoints;
    };
    match ctx
        .viz
        .emit(TOOL, PREMIUM_VIZ, spec, &chart.title, chart.source)
    {
        true => Delivered::Drawn,
        false => Delivered::NoSink,
    }
}

/// Python's `int(row.get("time") or 0)`.
fn epoch(value: Option<&Value>) -> i64 {
    value
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.trunc() as i64)))
        .unwrap_or(0)
}

/// "strike" or "strikes", "session" or "sessions".
fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        one.to_string()
    } else {
        format!("{}s", one)
    }
}

/// The rolling ATM straddle, computed by the Straddle page's service: the
/// ATM strike is recomputed from the underlying close for every candle.
pub async fn rolling_atm(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    req: Request,
) -> Result<ToolOutcome, ToolOutcome> {
    let Request {
        interval,
        sessions,
        overlays,
        mut notices,
    } = req;
    let base = normalise_symbol(Some(&arg_or(args, "underlying", json!(""))), "underlying")?;
    let listed = master(&ctx.app);
    let venue = resolve_underlying_exchange(&listed, &base, args.get("exchange"), &mut notices)?;
    let expiry = resolve_expiry(ctx, &base, &venue, args.get("expiry_date"), &mut notices).await?;
    let payload = tools_service(
        "straddle_chart_service.get_straddle_chart_data",
        history_tools_service::straddle_chart(
            &ctx.app, &base, &venue, &expiry, &interval, sessions,
        ),
    )
    .await?;
    let series: Vec<Value> = payload
        .get("data")
        .and_then(|d| d.get("series"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter(|r| r.is_object()).cloned().collect())
        .unwrap_or_default();
    let labels = [label("underlying", &base), label("expiry", &expiry)];
    if series.is_empty() {
        return Ok(tool_answer(
            TOOL,
            &format!(
                "No rolling ATM straddle came back for {} {} at {}, so nothing was drawn. Option history may be missing for the strikes that were ATM during the range.",
                base, expiry, interval
            ),
            &notices,
            &labels,
        ));
    }
    let mut strikes: Vec<f64> = series
        .iter()
        .filter_map(|r| r.get("atm_strike").and_then(as_number))
        .filter(|k| *k != 0.0)
        .collect();
    strikes.sort_by(f64::total_cmp);
    strikes.dedup();
    let strike_list = strikes
        .iter()
        .map(|k| format_number(&json!(k)))
        .collect::<Vec<_>>()
        .join(", ");
    let chart = PremiumChart {
        times: series.iter().map(|r| epoch(r.get("time"))).collect(),
        combined: series
            .iter()
            .map(|r| r.get("straddle").and_then(as_number))
            .collect(),
        title: format!("{} {} rolling ATM straddle, {}", base, expiry, interval),
        subtitle: format!(
            "Rolling ATM: the strike follows spot. {} ATM {} used ({}).",
            strikes.len(),
            plural(strikes.len(), "strike"),
            strike_list
        ),
        axis: "Straddle premium",
        interval: &interval,
        indicators: &overlays,
        source: "straddle_chart_service",
    };
    match deliver_premium(ctx, &chart, &mut notices) {
        Delivered::Drawn => {}
        Delivered::NoSink => {
            return Ok(tool_answer(TOOL, &no_sink_message("chart"), &notices, &labels[..1]))
        }
        Delivered::NoPoints => {
            return Ok(tool_answer(
                TOOL,
                &format!(
                    "The rolling ATM straddle for {} {} at {} came back without a single priced point, so nothing was drawn. Option history may be missing for the strikes that were ATM during the range.",
                    base, expiry, interval
                ),
                &notices,
                &labels,
            ))
        }
    }
    let last = &series[series.len() - 1];
    let at = |k: &str| last.get(k).cloned().unwrap_or(Value::Null);
    let message = format!(
        "Drew the rolling ATM straddle for {} {} at {}, {} points across {} {}. The strike follows spot, so this is the volatility view rather than a fixed position: {} ATM {} were used ({}). Last point {} at strike {} with spot {}: call {} plus put {}.",
        base, expiry, interval, series.len(), sessions, plural(sessions as usize, "session"),
        strikes.len(), plural(strikes.len(), "strike"), strike_list,
        format_price(&at("straddle")), format_number(&at("atm_strike")), format_price(&at("spot")),
        format_price(&at("ce_price")), format_price(&at("pe_price"))
    );
    Ok(tool_answer(TOOL, &message, &notices, &labels))
}

/// One fixed leg of a combined premium.
#[derive(Debug, Clone)]
pub struct FixedLeg {
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub lots: i64,
}

/// The closes every leg printed at the same timestamps, inner-joined, and
/// the notices naming what each leg lost to the join.
pub fn aligned(
    legs: &[FixedLeg],
    closes: &[BTreeMap<i64, f64>],
    notices: &mut Vec<String>,
) -> (Vec<i64>, usize) {
    let mut common: BTreeSet<i64> = closes
        .first()
        .map(|c| c.keys().copied().collect())
        .unwrap_or_default();
    for seen in closes.iter().skip(1) {
        common.retain(|t| seen.contains_key(t));
    }
    let mut dropped_total = 0;
    for (leg, seen) in legs.iter().zip(closes) {
        let dropped = seen.len().saturating_sub(common.len());
        dropped_total += dropped;
        if dropped > 0 {
            notices.push(format!(
                "{} printed {} candles, {} of which had no matching candle on every other leg and were dropped from the combined series.",
                leg.symbol,
                seen.len(),
                dropped
            ));
        }
    }
    (common.into_iter().collect(), dropped_total)
}

/// The combined value at each time: the signed multiplier times each leg's
/// close, summed in leg order, rounded to 4 places.
pub fn combine(legs: &[FixedLeg], closes: &[BTreeMap<i64, f64>], times: &[i64]) -> Vec<f64> {
    times
        .iter()
        .map(|t| {
            let sum = legs.iter().zip(closes).fold(0.0, |acc, (leg, seen)| {
                acc + signed_multiplier(&leg.side, leg.lots) as f64
                    * seen.get(t).copied().unwrap_or(0.0)
            });
            py_round(sum, 4)
        })
        .collect()
}

fn label_of(leg: &FixedLeg) -> String {
    let mut named = Map::new();
    named.insert("side".into(), json!(leg.side));
    named.insert("lots".into(), json!(leg.lots));
    named.insert("symbol".into(), json!(leg.symbol));
    leg_label(&named)
}

/// Read each named leg against the instrument master.
async fn resolve_fixed_legs(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    entries: &[Value],
    notices: &mut Vec<String>,
) -> Result<Vec<FixedLeg>, ToolOutcome> {
    let base = truthy_text(args.get("underlying")).trim().to_uppercase();
    let mut shorthand_expiry = truthy_text(args.get("expiry_date")).trim().to_uppercase();
    if !shorthand_expiry.is_empty() {
        shorthand_expiry = normalise_leg_expiry(Some(&json!(shorthand_expiry)), "expiry_date")?;
    }
    let listed = master(&ctx.app);
    let mut legs = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let position = i + 1;
        let place = format!("leg {}", position);
        let (symbol, named) = leg_symbol(entry, position, &base, &shorthand_expiry)?;
        let exchange = resolve_leg_exchange(&listed, &symbol, &named, &place)?;
        segment_of(&symbol, Some(&place))?;
        let (side, lots) = leg_side_and_lots(
            entry,
            position,
            || master_lot_size(ctx, &symbol, &exchange),
            notices,
        )
        .await?;
        legs.push(FixedLeg {
            symbol,
            exchange,
            side: side.to_string(),
            lots,
        });
    }
    Ok(legs)
}

/// One history frame per leg (different instruments, nothing fetched
/// twice), as closes by time.
async fn leg_closes(
    ctx: &ToolCtx,
    legs: &[FixedLeg],
    interval: &str,
    (start, end): (&str, &str),
) -> Result<Vec<BTreeMap<i64, f64>>, ToolOutcome> {
    let mut closes = Vec::new();
    for leg in legs {
        let mut request = Map::new();
        request.insert("symbol".into(), json!(leg.symbol));
        request.insert("exchange".into(), json!(leg.exchange));
        request.insert("interval".into(), json!(interval));
        request.insert("start_date".into(), json!(start));
        request.insert("end_date".into(), json!(end));
        let response = ctx
            .service_read("history_service.get_history", "history", request)
            .await?;
        let records = history_records(response);
        let columns = candle_columns(records.first().unwrap_or(&Map::new()));
        let bars = chart_bars(&records, &columns);
        closes.push(
            bars.iter()
                .filter_map(|b| Some((b.get("time")?.as_i64()?, as_number(b.get("close")?)?)))
                .collect(),
        );
    }
    Ok(closes)
}

/// The answer once the fixed-leg series is on screen.
fn fixed_message(
    legs: &[FixedLeg],
    closes: &[BTreeMap<i64, f64>],
    times: &[i64],
    combined: &[f64],
    (interval, start, end): (&str, &str, &str),
    dropped: usize,
) -> String {
    let last = times.last().copied().unwrap_or(0);
    let parts = legs
        .iter()
        .zip(closes)
        .map(|(leg, seen)| {
            format!(
                "{} at {}",
                label_of(leg),
                price(seen.get(&last).copied().unwrap_or(0.0))
            )
        })
        .collect::<Vec<_>>()
        .join(" plus ");
    format!(
        "Drew the combined premium of {} fixed legs at {} from {} to {}: {} aligned bars, {} dropped. The strikes are held constant, so this is a fixed position rather than the rolling ATM view. Last bar {} ({}).",
        legs.len(), interval, start, end, times.len(), dropped,
        price(combined.last().copied().unwrap_or(0.0)), parts
    )
}

/// The contracts the operator named, summed, strikes held constant.
pub async fn fixed_legs(
    ctx: &ToolCtx,
    args: &Map<String, Value>,
    entries: &[Value],
    req: Request,
) -> Result<ToolOutcome, ToolOutcome> {
    let Request {
        interval,
        sessions,
        overlays,
        mut notices,
    } = req;
    let legs = resolve_fixed_legs(ctx, args, entries, &mut notices).await?;
    let (from, to) = trading_window(today_ist(&ctx.app), sessions);
    let (start, end) = (
        from.format("%Y-%m-%d").to_string(),
        to.format("%Y-%m-%d").to_string(),
    );

    let closes = leg_closes(ctx, &legs, &interval, (&start, &end)).await?;
    let empty: Vec<&str> = legs
        .iter()
        .zip(&closes)
        .filter(|(_, c)| c.is_empty())
        .map(|(l, _)| l.symbol.as_str())
        .collect();
    if !empty.is_empty() {
        return Ok(tool_answer(
            TOOL,
            &format!(
                "No candles came back for {} at {} between {} and {}, so nothing was drawn. Check the contract is listed and still trading, and that the interval is one this broker supports.",
                empty.join(", "), interval, start, end
            ),
            &notices,
            &[],
        ));
    }
    let (common, dropped) = aligned(&legs, &closes, &mut notices);
    if common.is_empty() {
        return Ok(tool_answer(
            TOOL,
            &format!(
                "The legs share no candle timestamp at {} between {} and {}, so no combined series could be built. One of them is illiquid enough that it never printed at the same time as the others; try a coarser interval.",
                interval, start, end
            ),
            &notices,
            &[],
        ));
    }
    let times = cap_last_n_dates(common, sessions, |t| *t);
    let combined = combine(&legs, &closes, &times);
    let heading = if legs.len() <= MAX_HEADING_LEGS {
        legs.iter().map(label_of).collect::<Vec<_>>().join(" ")
    } else {
        format!("{} and {} more legs", legs[0].symbol, legs.len() - 1)
    };
    let chart = PremiumChart {
        times: times.clone(),
        combined: combined.iter().map(|v| Some(*v)).collect(),
        title: format!("{}, combined premium at {}", heading, interval),
        subtitle: format!(
            "Fixed legs: strikes held constant. {} aligned bars, {} dropped.",
            times.len(),
            dropped
        ),
        axis: "Combined premium",
        interval: &interval,
        indicators: &overlays,
        source: "history_service",
    };
    match deliver_premium(ctx, &chart, &mut notices) {
        Delivered::Drawn => {}
        Delivered::NoSink => return Ok(tool_answer(TOOL, &no_sink_message("chart"), &notices, &[])),
        Delivered::NoPoints => {
            return Ok(tool_answer(
                TOOL,
                &format!(
                    "The combined premium at {} between {} and {} came back without a single priced point, so nothing was drawn.",
                    interval, start, end
                ),
                &notices,
                &[],
            ))
        }
    }
    let message = fixed_message(
        &legs,
        &closes,
        &times,
        &combined,
        (&interval, &start, &end),
        dropped,
    );
    Ok(tool_answer(TOOL, &message, &notices, &[]))
}

#[cfg(test)]
pub(super) fn ist_seconds(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
    use chrono::TimeZone;
    crate::agent::tools::market::IST
        .with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .map(|t| t.timestamp())
        .unwrap_or(0)
}
