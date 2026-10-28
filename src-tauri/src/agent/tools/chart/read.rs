//! `read_chart` and `analyse_chart` (the web's `ChartToolkit.read_chart`,
//! `analyse_chart`, `_momentum`, `_summary` and `_momentum_verdict`). Both
//! draw nothing.

use super::window::{window, Meta, Window};
use super::{ist, no_chart, At, ChartRun};
use crate::agent::chart_contract::{ChartView, IndicatorOnChart};
use crate::agent::chart_geometry::{bar_seconds, significant_pivots, structure, Fit};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::indicators::{compute, Json};
use crate::agent::tools::answer::{json_result_ordered, label};
use crate::agent::tools::numbers::{price, py_round};
use crate::agent::tools::values::float_repr;
use serde::Serialize;
use serde_json::{Map, Value};

fn ist_of(seconds: Option<f64>) -> String {
    seconds.map(ist).unwrap_or_default()
}

#[derive(Serialize)]
struct TraderDrawingOut {
    tool: String,
    anchors: Vec<At>,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
}

#[derive(Serialize)]
struct ReadOut {
    symbol: String,
    exchange: String,
    interval: String,
    chart_type: String,
    bars_loaded: u64,
    visible_bars: u64,
    visible_from: String,
    visible_to: String,
    last_price: Option<f64>,
    timezone: &'static str,
    indicators: Vec<IndicatorOnChart>,
    operator_drawings: Vec<TraderDrawingOut>,
    your_drawing_groups: Vec<&'static str>,
    summary: String,
}

/// What is on the chart right now, from the context alone: no fetch, no
/// frame (`read_chart`).
pub(super) fn read_chart(run: &ChartRun) -> ToolOutcome {
    let view = run.view();
    if !view.is_open() {
        return no_chart("read_chart");
    }
    let payload = ReadOut {
        symbol: view.symbol.clone(),
        exchange: view.exchange.clone(),
        interval: view.interval.clone(),
        chart_type: if view.chart_type.is_empty() {
            "candlestick".into()
        } else {
            view.chart_type.clone()
        },
        bars_loaded: view.bars_loaded,
        visible_bars: view.visible_bars,
        visible_from: ist_of(view.visible_from),
        visible_to: ist_of(view.visible_to),
        last_price: view.last_price,
        timezone: "Asia/Kolkata",
        indicators: view.indicators.clone(),
        operator_drawings: view
            .drawings
            .iter()
            .map(|d| TraderDrawingOut {
                tool: d.tool.clone(),
                anchors: d
                    .points
                    .iter()
                    .map(|p| At {
                        at: ist(p.time),
                        price: p.price,
                    })
                    .collect(),
                text: d.text.clone(),
            })
            .collect(),
        your_drawing_groups: view.agent_groups.iter().map(|g| g.as_str()).collect(),
        summary: summary(&view),
    };
    json_result_ordered("read_chart", &payload, &[])
}

/// The context as one sentence (`_summary`).
fn summary(view: &ChartView) -> String {
    let mut parts = vec![format!(
        "{} on {}, {}.",
        view.symbol, view.exchange, view.interval
    )];
    if view.bars_loaded > 0 {
        parts.push(format!(
            "{} bars loaded, {} visible.",
            view.bars_loaded, view.visible_bars
        ));
    }
    if let Some(p) = view.last_price {
        parts.push(format!("Last price {}.", price(p)));
    }
    if !view.indicators.is_empty() {
        let names: Vec<&str> = view.indicators.iter().map(|i| i.name.as_str()).collect();
        parts.push(format!("Indicators: {}.", names.join(", ")));
    }
    if !view.drawings.is_empty() {
        parts.push(format!(
            "{} drawing(s) the operator placed by hand.",
            view.drawings.len()
        ));
    }
    if !view.agent_groups.is_empty() {
        let groups: Vec<&str> = view.agent_groups.iter().map(|g| g.as_str()).collect();
        parts.push(format!("Your markup on screen: {}.", groups.join(", ")));
    }
    parts.join(" ")
}

#[derive(Serialize)]
struct SwingCounts {
    higher_highs: usize,
    lower_highs: usize,
    higher_lows: usize,
    lower_lows: usize,
}

#[derive(Serialize)]
struct RailOut {
    from: At,
    to: At,
    slope_per_bar: f64,
    touches: usize,
    r2: f64,
    breaks: usize,
}

#[derive(Serialize)]
struct Rails {
    #[serde(skip_serializing_if = "Option::is_none")]
    resistance: Option<RailOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    support: Option<RailOut>,
}

#[derive(Serialize)]
struct AnalyseOut {
    #[serde(flatten)]
    meta: Meta,
    direction: &'static str,
    structure: &'static str,
    swing_counts: SwingCounts,
    slope_per_bar: f64,
    rails: Option<Rails>,
    period_high: f64,
    period_low: f64,
    last_close: f64,
    period_change_pct: Option<f64>,
    swing_highs: Vec<f64>,
    swing_lows: Vec<f64>,
    momentum: Json,
}

fn rail_out(fit: &Fit, seconds: f64) -> RailOut {
    RailOut {
        from: At {
            at: ist(fit.from.time),
            price: fit.from.price,
        },
        to: At {
            at: ist(fit.to.time),
            price: fit.to.price,
        },
        slope_per_bar: py_round(fit.slope * seconds, 4),
        touches: fit.touches,
        r2: fit.r2,
        breaks: fit.breaks,
    }
}

/// Trend, structure and momentum from one candle fetch
/// (`analyse_chart`).
pub(super) async fn analyse_chart(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("analyse_chart"));
    }
    let w = window(run, ctx, &view, args.get("lookback_bars")).await?;
    let (bars, lo, hi) = (w.bars(), w.lo, w.hi);
    let (highs, lows) = significant_pivots(bars, lo, hi);
    let shape = structure(&highs, &lows);
    let seconds = bar_seconds(bars);
    let fits = super::draw::rails(bars, &highs, &lows);
    let slopes: Vec<f64> = fits.iter().map(|(_, f)| f.slope * seconds).collect();
    let rail = |name: &str| {
        fits.iter()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, f)| rail_out(f, seconds))
    };
    let average = (!slopes.is_empty()).then(|| slopes.iter().sum::<f64>() / slopes.len() as f64);
    let direction = match average {
        None => "sideways",
        Some(avg) => {
            let span = bars.span(lo, hi);
            let drift = avg * (hi - lo) as f64;
            if drift > 0.15 * span {
                "up"
            } else if drift < -0.15 * span {
                "down"
            } else {
                "sideways"
            }
        }
    };
    let (first_close, last_close) = (bars.closes[lo], bars.closes[hi]);
    let (period_high, period_low) = bars.extremes(lo, hi).unwrap_or_default();
    let last_six = |pivots: &[crate::agent::chart_geometry::Pivot]| -> Vec<f64> {
        let skip = pivots.len().saturating_sub(6);
        pivots[skip..]
            .iter()
            .map(|p| py_round(p.price, 4))
            .collect()
    };
    let payload = AnalyseOut {
        direction,
        structure: shape.verdict,
        swing_counts: SwingCounts {
            higher_highs: shape.higher_highs,
            lower_highs: shape.lower_highs,
            higher_lows: shape.higher_lows,
            lower_lows: shape.lower_lows,
        },
        slope_per_bar: average.map_or(0.0, |a| py_round(a, 4)),
        rails: (!fits.is_empty()).then(|| Rails {
            resistance: rail("resistance"),
            support: rail("support"),
        }),
        period_high: py_round(period_high, 4),
        period_low: py_round(period_low, 4),
        last_close: py_round(last_close, 4),
        period_change_pct: (first_close != 0.0)
            .then(|| py_round((last_close - first_close) / first_close * 100.0, 2)),
        swing_highs: last_six(&highs),
        swing_lows: last_six(&lows),
        momentum: momentum(&w),
        meta: w.meta.clone(),
    };
    Ok(json_result_ordered(
        "analyse_chart",
        &payload,
        &[label("symbol", &view.symbol)],
    ))
}

/// The momentum indicators of an analysis, over the same candles.
const MOMENTUM: [&str; 4] = ["rsi", "macd", "adx", "atr"];

/// RSI, MACD, ADX and ATR over the fetch the rest of the analysis
/// describes, each output's latest value and direction, and a one-line
/// verdict; an indicator that could not be computed is named in
/// `unavailable`, never silently missing (`_momentum`).
fn momentum(w: &Window) -> Json {
    let mut readings: Vec<(String, Json)> = Vec::new();
    let mut unavailable: Vec<(String, Json)> = Vec::new();
    for name in MOMENTUM {
        match compute(name, &w.fetched.frame, None, None, 3) {
            Ok(result) => {
                let outputs = result.outputs.iter().map(|output| {
                    let stats = result
                        .summary
                        .iter()
                        .find(|(k, _)| k == output)
                        .map(|(_, s)| s);
                    let field = |key: &str| {
                        stats
                            .and_then(|s| s.get(key))
                            .cloned()
                            .unwrap_or(Json::Null)
                    };
                    (
                        output.clone(),
                        Json::obj([
                            ("latest", field("latest")),
                            ("direction", field("direction")),
                        ]),
                    )
                });
                readings.push((name.to_string(), Json::obj(outputs)));
            }
            Err(e) => unavailable.push((name.to_string(), Json::str(e.to_string()))),
        }
    }
    let verdict = momentum_verdict(&readings);
    let mut block = Json::obj([
        ("bars_used", Json::Int(w.bars().len() as i64)),
        ("readings", Json::Obj(readings)),
        ("verdict", Json::str(verdict)),
    ]);
    if !unavailable.is_empty() {
        block.set("unavailable", Json::Obj(unavailable));
    }
    block
}

fn number(json: Option<&Json>) -> Option<f64> {
    match json? {
        Json::Num(v) if v.is_finite() => Some(*v),
        Json::Int(v) => Some(*v as f64),
        _ => None,
    }
}

/// The `latest` of the first output of `name` whose label passes `pick`.
fn latest(readings: &[(String, Json)], name: &str, pick: impl Fn(&str) -> bool) -> Option<f64> {
    let (_, outputs) = readings.iter().find(|(n, _)| n == name)?;
    let Json::Obj(pairs) = outputs else {
        return None;
    };
    let (_, reading) = pairs
        .iter()
        .find(|(label, _)| pick(&label.to_lowercase()))?;
    number(reading.get("latest"))
}

/// The readings as one plain sentence (`_momentum_verdict`).
fn momentum_verdict(readings: &[(String, Json)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = latest(readings, "rsi", |_| true) {
        let state = if v >= 70.0 {
            "overbought"
        } else if v <= 30.0 {
            "oversold"
        } else {
            "neutral"
        };
        parts.push(format!("RSI {} is {}", float_repr(py_round(v, 2)), state));
    }
    let line = latest(readings, "macd", |k| k.contains("macd"));
    let signal = latest(readings, "macd", |k| k.contains("signal"));
    if let (Some(line), Some(signal)) = (line, signal) {
        parts.push(if line > signal {
            "MACD is above its signal".into()
        } else {
            "MACD is below its signal".into()
        });
    }
    if let Some(v) = latest(readings, "adx", |_| true) {
        let strength = if v >= 25.0 { "strong" } else { "weak" };
        parts.push(format!(
            "ADX {} shows a {} trend",
            float_repr(py_round(v, 2)),
            strength
        ));
    }
    if parts.is_empty() {
        "no reading was conclusive".into()
    } else {
        parts.join("; ")
    }
}
