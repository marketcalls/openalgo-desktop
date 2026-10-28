//! The tools that mark the chart up (the web's `ChartToolkit.find_patterns`,
//! `draw_levels`, `draw_trendline`, `draw_zone`, `clear_drawings`,
//! `_note`, `_caption`, `_rails` and `_markers`).
//!
//! Every price drawn comes from `chart_geometry`; marking is not optional,
//! and a draw replaces its whole group.

use super::window::{window, Meta};
use super::{answer, choice, ist, no_chart, At, ChartRun, NO_SINK};
use crate::agent::chart_contract::{self as cc, Anchor, Group, Shape, MAX_NOTE_CHARS};
use crate::agent::chart_geometry::{
    bar_seconds, candlestick_patterns, chart_patterns, consolidation, fit_line, significant_pivots,
    support_resistance, zone, Bars, CandleHit, Fit, Pivot, Rail, ZoneKind, MIN_RANGE_BARS,
    TOUCH_TOLERANCE_PCT,
};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::tools::answer::{json_result_ordered, label};
use crate::agent::tools::numbers::{as_number, price, py_round};
use crate::agent::tools::values::{float_repr, py_repr, py_str, truthy};
use crate::agent::websearch::taint::{constrain, Decision};
use serde::Serialize;
use serde_json::{Map, Value};

/// Levels one call may draw: more than eight lines is a grid.
const MAX_LEVELS: usize = 8;

/// Candlestick markers one call may place, the most recent.
const MAX_MARKERS: usize = 8;

/// Candlestick patterns are looked for over this many bars at the right
/// edge of the viewport, whatever the lookback: the lookback sizes the
/// structure, not the signal.
const CANDLE_SCAN_BARS: usize = 30;

const TREND_SIDES: [&str; 4] = ["auto", "support", "resistance", "both"];
const ZONE_KINDS: [&str; 4] = ["demand", "supply", "both", "range"];

/// A channel is both rails.
const TREND_SIDE_ALIASES: [(&str, &str); 1] = [("channel", "both")];

/// The words `draw_zone` tells the model to answer with `range`.
const ZONE_KIND_ALIASES: [(&str, &str); 5] = [
    ("consolidation", "range"),
    ("sideways", "range"),
    ("base", "range"),
    ("box", "range"),
    ("accumulation", "range"),
];

/// Singular and plural spellings of each group; "all" and "everything"
/// clear every Agent group.
const GROUP_ALIASES: [(&str, &str); 6] = [
    ("level", "levels"),
    ("zones", "zone"),
    ("pattern", "patterns"),
    ("trendlines", "trendline"),
    ("all", ""),
    ("everything", ""),
];

/// The resistance and support rails of one window, fitted the one way both
/// line tools fit them (`_rails`): containment decides, so a resistance
/// rail has nothing above it from its anchor on. A side with fewer than two
/// swings is absent.
pub(super) fn rails(bars: &Bars, highs: &[Pivot], lows: &[Pivot]) -> Vec<(Rail, Fit)> {
    [(Rail::Resistance, highs), (Rail::Support, lows)]
        .into_iter()
        .filter_map(|(rail, pivots)| {
            fit_line(pivots, TOUCH_TOLERANCE_PCT, Some((bars, rail))).map(|f| (rail, f))
        })
        .collect()
}

/// A model-supplied caption reduced to the trader's own words (`_note`):
/// every digit removed first, then only the tokens of the trader's message
/// for this turn kept (the web search taint boundary). Nothing surviving
/// is no caption, never the trader's whole message.
fn caption_of(ctx: &ToolCtx, note: Option<&Value>) -> String {
    let cleaned = cc::safe_note(note.unwrap_or(&Value::Null));
    if cleaned.is_empty() {
        return String::new();
    }
    let kept = constrain(&cleaned, &ctx.operator_message);
    match kept.decision {
        Decision::Verbatim | Decision::Constrained => {
            kept.text.chars().take(MAX_NOTE_CHARS).collect()
        }
        Decision::OperatorMessage | Decision::Blocked => String::new(),
    }
}

/// The computed label, with the caption on the first shape of a group only
/// (`_caption`).
fn caption(computed: &str, note: &str, index: usize) -> String {
    if !note.is_empty() && index == 0 {
        format!("{} - {}", computed, note)
    } else {
        computed.to_string()
    }
}

#[derive(Serialize)]
struct CandleOut {
    name: &'static str,
    bias: &'static str,
    at: String,
    price: f64,
    bars: usize,
}

#[derive(Serialize)]
struct PatternOut {
    name: &'static str,
    bias: &'static str,
    anchors: Vec<At>,
}

#[derive(Serialize)]
struct PatternsOut {
    #[serde(flatten)]
    meta: Meta,
    candlestick_patterns: Vec<CandleOut>,
    candlestick_hits_found: usize,
    chart_patterns: Vec<PatternOut>,
    drawn: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
}

/// One marker per bar (`_markers`): the patterns that printed on one
/// candle become one label, seated where the first directional bias puts
/// it.
fn markers(hits: &[CandleHit]) -> Vec<Option<Shape>> {
    let mut bars: Vec<(f64, f64, Vec<&str>, &str)> = Vec::new();
    for hit in hits {
        match bars.iter_mut().find(|b| b.0 == hit.time) {
            None => bars.push((hit.time, hit.price, vec![hit.name], hit.bias)),
            Some(bar) => {
                bar.2.push(hit.name);
                if bar.3 == "neutral" && hit.bias != "neutral" {
                    bar.3 = hit.bias;
                    bar.1 = hit.price;
                }
            }
        }
    }
    bars.into_iter()
        .map(|(time, at, names, tone)| cc::marker(Anchor::new(time, at), &names.join(", "), tone))
        .collect()
}

/// Candlestick and chart patterns, every one marked on the chart
/// (`find_patterns`). A triangle or a rectangle is drawn as its two rails
/// (D-029).
pub(super) async fn find_patterns(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("find_patterns"));
    }
    let w = window(run, ctx, &view, args.get("lookback_bars")).await?;
    let (bars, lo, hi) = (w.bars(), w.lo, w.hi);
    let (highs, lows) = significant_pivots(bars, lo, hi);
    let candles = candlestick_patterns(bars, CANDLE_SCAN_BARS, lo, hi);
    let found = chart_patterns(bars, &highs, &lows, lo, hi);
    let recent = &candles[candles.len().saturating_sub(MAX_MARKERS)..];

    let mut shapes: Vec<Option<Shape>> = Vec::new();
    for pattern in &found {
        for (k, [a, b]) in pattern.segments.iter().enumerate() {
            let label = if k == 0 { pattern.name } else { "" };
            let (from, to) = (
                pattern.anchors.get(*a).copied(),
                pattern.anchors.get(*b).copied(),
            );
            shapes.push(cc::trendline(from, to, false, label, pattern.bias));
        }
    }
    shapes.extend(markers(recent));
    let drawn = run.emit(ctx, vec![cc::draw(Group::Patterns, shapes)]);

    let payload = PatternsOut {
        meta: w.meta.clone(),
        candlestick_patterns: recent
            .iter()
            .map(|h| CandleOut {
                name: h.name,
                bias: h.bias,
                at: ist(h.time),
                price: h.price,
                bars: h.bars,
            })
            .collect(),
        candlestick_hits_found: candles.len(),
        chart_patterns: found
            .iter()
            .map(|p| PatternOut {
                name: p.name,
                bias: p.bias,
                anchors: p
                    .anchors
                    .iter()
                    .map(|a| At {
                        at: ist(a.time),
                        price: a.price,
                    })
                    .collect(),
            })
            .collect(),
        drawn,
        note: (!drawn).then_some(NO_SINK),
    };
    Ok(json_result_ordered(
        "find_patterns",
        &payload,
        &[label("symbol", &view.symbol)],
    ))
}

/// The strongest support and resistance levels, drawn (`draw_levels`).
pub(super) async fn draw_levels(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("draw_levels"));
    }
    let wanted = args
        .get("count")
        .filter(|v| truthy(v))
        .and_then(as_number)
        .map_or(4.0, f64::trunc)
        .clamp(1.0, MAX_LEVELS as f64) as usize;
    let w = window(run, ctx, &view, args.get("lookback_bars")).await?;
    let reference = view.last_price.unwrap_or(w.bars().closes[w.hi]);
    let mut found = support_resistance(w.bars(), w.lo, w.hi, Some(reference), 2);
    found.truncate(wanted);
    let searched = w.meta.bars_analysed;
    if found.is_empty() {
        return Ok(answer(
            "draw_levels",
            &format!(
                "No price on {} {} was turned at twice in the {} bars searched, so there is no level worth drawing. Say so, and offer a longer lookback rather than drawing something arbitrary.",
                view.symbol, view.interval, searched
            ),
            None,
        ));
    }
    let note = caption_of(ctx, args.get("note"));
    let shapes: Vec<Option<Shape>> = found
        .iter()
        .enumerate()
        .map(|(index, l)| {
            let tone = if l.role == "resistance" {
                "bearish"
            } else {
                "bullish"
            };
            let text = caption(&format!("{} {}", l.role, price(l.price)), &note, index);
            cc::level(l.price, Some(l.first_time), &text, tone)
        })
        .collect();
    let drawn = shapes.len();
    if !run.emit(ctx, vec![cc::draw(Group::Levels, shapes)]) {
        return Ok(answer("draw_levels", NO_SINK, None));
    }
    let described: Vec<String> = found
        .iter()
        .map(|l| format!("{} ({}, {} touches)", price(l.price), l.role, l.touches))
        .collect();
    Ok(answer(
        "draw_levels",
        &format!(
            "Drew {} levels on {} {} in your 'levels' group, from {} bars: {}. The operator can see them, so say what they mean rather than listing them again.",
            drawn,
            view.symbol,
            view.interval,
            searched,
            described.join(", ")
        ),
        Some(&view.symbol),
    ))
}

/// The rail or rails to draw for `wanted`, or the refusal when that side
/// cannot be fitted.
fn chosen_rails<'a>(
    wanted: &str,
    fits: &'a [(Rail, Fit)],
    refuse: impl Fn(Rail) -> ToolOutcome,
) -> Result<Vec<&'a (Rail, Fit)>, ToolOutcome> {
    match wanted {
        "both" => Ok(fits.iter().collect()),
        "auto" => {
            let mut best: Option<&(Rail, Fit)> = None;
            for fit in fits {
                let better =
                    best.is_none_or(|(_, b)| (fit.1.touches, fit.1.r2) > (b.touches, b.r2));
                if better {
                    best = Some(fit);
                }
            }
            Ok(best.into_iter().collect())
        }
        side => match fits.iter().find(|(r, _)| r.as_str() == side) {
            Some(fit) => Ok(vec![fit]),
            None => Err(refuse(fits[0].0)),
        },
    }
}

/// A trendline through real swings, or both rails of a channel
/// (`draw_trendline`).
pub(super) async fn draw_trendline(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("draw_trendline"));
    }
    let wanted = choice(
        "side",
        args.get("side"),
        "auto",
        &TREND_SIDES,
        &TREND_SIDE_ALIASES,
    )?;
    let w = window(run, ctx, &view, args.get("lookback_bars")).await?;
    let (highs, lows) = significant_pivots(w.bars(), w.lo, w.hi);
    let seconds = bar_seconds(w.bars());
    let fits = rails(w.bars(), &highs, &lows);
    let searched = w.meta.bars_analysed;
    if fits.is_empty() {
        return Ok(answer(
            "draw_trendline",
            &format!(
                "Fewer than two swing points on {} {} in the {} bars searched, so there is nothing to draw a line through. Ask for a longer lookback rather than drawing a guess.",
                view.symbol, view.interval, searched
            ),
            None,
        ));
    }
    let chosen = match chosen_rails(wanted, &fits, |other| {
        answer(
            "draw_trendline",
            &format!(
                "There are fewer than two swing {} on {} {} in the {} bars searched, so a {} line cannot be fitted. A {} line can; ask the operator whether they want that instead.",
                if wanted == "support" { "lows" } else { "highs" },
                view.symbol,
                view.interval,
                searched,
                wanted,
                other.as_str()
            ),
            None,
        )
    }) {
        Ok(c) => c,
        Err(refusal) => return Ok(refusal),
    };
    let note = caption_of(ctx, args.get("note"));
    let mut shapes = Vec::new();
    let mut described = Vec::new();
    for (index, (rail, fit)) in chosen.into_iter().enumerate() {
        let per_bar = fit.slope * seconds;
        let tone = if per_bar > 0.0 {
            "bullish"
        } else if per_bar < 0.0 {
            "bearish"
        } else {
            "neutral"
        };
        let text = caption(rail.as_str(), &note, index);
        shapes.push(cc::trendline(
            Some(fit.from.anchor()),
            Some(fit.to.anchor()),
            true,
            &text,
            tone,
        ));
        described.push(format!(
            "{} from {} on {} to {} on {}, {} touches, r2 {}, {} bars pierced it since, {} per bar",
            rail.as_str(),
            price(fit.from.price),
            ist(fit.from.time),
            price(fit.to.price),
            ist(fit.to.time),
            fit.touches,
            float_repr(fit.r2),
            fit.breaks,
            float_repr(py_round(per_bar, 4))
        ));
    }
    let drawn = shapes.len();
    if !run.emit(ctx, vec![cc::draw(Group::Trendline, shapes)]) {
        return Ok(answer("draw_trendline", NO_SINK, None));
    }
    Ok(answer(
        "draw_trendline",
        &format!(
            "Drew {} rail(s) on {} {} in your 'trendline' group, extended right: {}.",
            drawn,
            view.symbol,
            view.interval,
            described.join("; ")
        ),
        Some(&view.symbol),
    ))
}

/// The shapes and descriptions `draw_zone` draws for `wanted`.
fn zone_shapes(
    bars: &Bars,
    lo: usize,
    hi: usize,
    wanted: &str,
    note: &str,
) -> (Vec<Option<Shape>>, Vec<String>) {
    let band = |from_time: f64, high: f64, to_time: f64, low: f64, text: &str, tone: &str| {
        cc::zone(
            Anchor::new(from_time, high),
            Anchor::new(to_time, low),
            text,
            tone,
        )
    };
    let mut shapes = Vec::new();
    let mut described = Vec::new();
    if wanted == "range" {
        if let Some(r) = consolidation(bars, lo, hi, MIN_RANGE_BARS) {
            shapes.push(band(
                r.from_time,
                r.high,
                r.to_time,
                r.low,
                &caption("range", note, 0),
                "neutral",
            ));
            described.push(format!(
                "consolidation between {} and {}, {} bars wide at {} percent, {} touches of an edge, price finishing {} percent of the band from where it started, from {} to {}",
                price(r.low),
                price(r.high),
                r.bars,
                float_repr(r.width_pct),
                r.touches,
                float_repr(r.drift_pct),
                ist(r.from_time),
                ist(r.to_time)
            ));
        }
        return (shapes, described);
    }
    let kinds: &[ZoneKind] = match wanted {
        "both" => &[ZoneKind::Demand, ZoneKind::Supply],
        "supply" => &[ZoneKind::Supply],
        _ => &[ZoneKind::Demand],
    };
    for (index, kind) in kinds.iter().enumerate() {
        let Some(z) = zone(bars, *kind, lo, hi) else {
            continue;
        };
        let tone = if *kind == ZoneKind::Demand {
            "bullish"
        } else {
            "bearish"
        };
        let text = caption(kind.as_str(), note, index);
        shapes.push(band(z.from_time, z.high, z.to_time, z.low, &text, tone));
        described.push(format!(
            "{} between {} and {}, off the swing at {} on {}, {} bars ago",
            kind.as_str(),
            price(z.low),
            price(z.high),
            price(z.pivot_price),
            ist(z.pivot_time),
            z.bars_since
        ));
    }
    (shapes, described)
}

/// A demand or supply zone off a real swing, or a consolidation
/// (`draw_zone`).
pub(super) async fn draw_zone(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("draw_zone"));
    }
    let wanted = choice(
        "kind",
        args.get("kind"),
        "demand",
        &ZONE_KINDS,
        &ZONE_KIND_ALIASES,
    )?;
    let w = window(run, ctx, &view, args.get("lookback_bars")).await?;
    let note = caption_of(ctx, args.get("note"));
    let (shapes, described) = zone_shapes(w.bars(), w.lo, w.hi, wanted, &note);
    if shapes.is_empty() {
        return Ok(answer(
            "draw_zone",
            &format!(
                "Nothing of that kind on {} {} in the {} bars searched: no significant swing for a demand or supply base, and no stretch that held one narrow band long enough to call a consolidation. Ask for a longer lookback rather than shading an arbitrary band.",
                view.symbol, view.interval, w.meta.bars_analysed
            ),
            None,
        ));
    }
    let drawn = shapes.len();
    if !run.emit(ctx, vec![cc::draw(Group::Zone, shapes)]) {
        return Ok(answer("draw_zone", NO_SINK, None));
    }
    Ok(answer(
        "draw_zone",
        &format!(
            "Shaded {} zone(s) on {} {} in your 'zone' group: {}.",
            drawn,
            view.symbol,
            view.interval,
            described.join("; ")
        ),
        Some(&view.symbol),
    ))
}

/// Remove the Agent's own markup, one group or all (`clear_drawings`).
/// Needs no chart: removing markup reads nothing.
pub(super) fn clear_drawings(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let given = args.get("group").filter(|v| truthy(v));
    let cleaned = given.map(py_str).unwrap_or_default().trim().to_lowercase();
    let wanted = GROUP_ALIASES
        .iter()
        .find(|(alias, _)| *alias == cleaned)
        .map_or(cleaned.as_str(), |(_, canonical)| *canonical);
    let group = Group::parse(wanted);
    if !wanted.is_empty() && group.is_none() {
        let names: Vec<&str> = Group::ALL.iter().map(|g| g.as_str()).collect();
        return Err(ToolOutcome::invalid_argument(
            "group",
            &format!(
                "{} is not one of your drawing groups",
                py_repr(given.unwrap_or(&Value::Null))
            ),
            Some(&format!(
                "Use one of: {}, or leave it empty to clear all of them.",
                names.join(", ")
            )),
        ));
    }
    if !run.emit(ctx, vec![cc::clear(group)]) {
        return Ok(answer("clear_drawings", NO_SINK, None));
    }
    Ok(answer(
        "clear_drawings",
        &match group {
            Some(g) => format!("Cleared your '{}' markup from the chart.", g.as_str()),
            None => "Cleared all of your markup from the chart. The operator's own drawings are untouched.".into(),
        },
        None,
    ))
}
