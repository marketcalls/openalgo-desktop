//! Chart geometry from real bars (the web's
//! `services/agent/chart_geometry.py`): swing points, levels, rails,
//! structure, zones and patterns.
//!
//! Pure functions on [`Bars`], read once from the candle frame the chart
//! tools fetched: no I/O, no clock, no logging. Every number they hand out
//! is a price that printed or a line through two prices that printed, so
//! the model never supplies one.
//!
//! - this file: the window, swing points (fractals, zigzag, the
//!   significance ladder), horizontal levels, fitted lines and the
//!   structure verdict;
//! - `zones`: demand and supply bands off a swing, and consolidations;
//! - `patterns`: candlestick patterns and chart patterns off the swings.
//!
//! Pivots anchor to the bar where the extreme printed, not the bar that
//! confirmed it; fractals are found over the whole fetch and then clipped
//! to the viewport, so a swing near the right edge is still confirmed by
//! bars the trader cannot see; significance is measured against the
//! visible range.

mod patterns;
mod zones;

#[cfg(test)]
pub(crate) mod tests;

pub use patterns::{candlestick_patterns, chart_patterns, CandleHit, ChartPattern};
pub use zones::{consolidation, zone, Range, ZoneBand, ZoneKind, MIN_RANGE_BARS};

use crate::agent::chart_contract::Anchor;
use crate::agent::tools::market::candles::CandleFrame;
use crate::agent::tools::numbers::py_round;
use std::cmp::Ordering;

/// Fractal half-widths: three bars either side ignores one outlier bar and
/// still catches a two-day reversal on a daily chart.
pub const DEFAULT_LEFT: usize = 3;
pub const DEFAULT_RIGHT: usize = 3;

/// Retracement thresholds tried in turn, as a fraction of the visible
/// range, strict to loose: a clean trend needs a loose threshold, a choppy
/// range a strict one. The last rung prunes nothing.
pub const SIGNIFICANCE_LADDER: [f64; 5] = [0.12, 0.09, 0.06, 0.03, 0.0];

/// Pivots per side [`significant_pivots`] tries to reach before it stops
/// relaxing.
pub const MIN_VERTICES: usize = 4;

/// How close a pivot sits to a line to count as a touch, as a percentage
/// of the line's price there.
pub const TOUCH_TOLERANCE_PCT: f64 = 0.5;

/// The pair search in [`fit_line`] is quadratic; beyond this many pivots
/// the newest are used.
pub const MAX_TRENDLINE_PIVOTS: usize = 40;

/// Which extreme a pivot is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    High,
    Low,
}

/// One confirmed swing point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pivot {
    /// Bar index in the [`Bars`] it came from.
    pub index: usize,
    /// UTC epoch seconds of that bar.
    pub time: f64,
    /// The high of a swing high, the low of a swing low.
    pub price: f64,
    pub kind: Side,
}

impl Pivot {
    /// The pivot as a chart anchor.
    pub fn anchor(&self) -> Anchor {
        Anchor {
            time: self.time,
            price: py_round(self.price, 4),
        }
    }
}

/// A cleaned candle window: times in UTC epoch seconds, ascending, one per
/// bar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bars {
    pub times: Vec<f64>,
    pub opens: Vec<f64>,
    pub highs: Vec<f64>,
    pub lows: Vec<f64>,
    pub closes: Vec<f64>,
}

impl Bars {
    /// The window of a candle frame (`Bars.from_frame`): a missing open,
    /// high or low falls back to the close; no close is an empty window.
    pub fn from_frame(frame: &CandleFrame) -> Bars {
        let Some(closes) = frame.column("close") else {
            return Bars::default();
        };
        let or_close = |name: &str| frame.column(name).unwrap_or(closes).to_vec();
        Bars {
            times: frame.time.iter().map(|t| *t as f64).collect(),
            opens: or_close("open"),
            highs: or_close("high"),
            lows: or_close("low"),
            closes: closes.to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.times.len()
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// The inclusive bar span inside a visible time range (`window`). A
    /// range selecting fewer than two bars falls back to the whole window:
    /// a viewport scrolled off the loaded data is not a reason to refuse.
    pub fn window(&self, start: Option<f64>, end: Option<f64>) -> (usize, usize) {
        let total = self.len();
        if total == 0 {
            return (0, 0);
        }
        let mut lo = 0;
        let mut hi = total - 1;
        if let Some(start) = start.filter(|s| s.is_finite()) {
            lo = self
                .times
                .iter()
                .position(|t| *t >= start)
                .unwrap_or(total - 1);
        }
        if let Some(end) = end.filter(|e| e.is_finite()) {
            hi = self.times.iter().rposition(|t| *t <= end).unwrap_or(0);
        }
        if hi < lo + 1 {
            return (0, total - 1);
        }
        (lo, hi)
    }

    /// The highest high and the lowest low across an inclusive bar span.
    pub fn extremes(&self, lo: usize, hi: usize) -> Option<(f64, f64)> {
        (lo <= hi && hi < self.len())
            .then(|| (max_of(&self.highs[lo..=hi]), min_of(&self.lows[lo..=hi])))
    }

    /// The high-to-low range across an inclusive bar span.
    pub fn span(&self, lo: usize, hi: usize) -> f64 {
        self.extremes(lo, hi).map_or(0.0, |(high, low)| high - low)
    }

    /// `(first, last)` of a span clipped to the window, `None` when empty.
    fn clip(&self, lo: usize, hi: usize) -> Option<(usize, usize)> {
        let last = hi.min(self.len().checked_sub(1)?);
        (lo <= last).then_some((lo, last))
    }
}

fn max_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn min_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::INFINITY, f64::min)
}

/// Python's float comparison for a sort or a ranking (NaN never wins).
fn cmp_f(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// Seconds between adjacent bars (`bar_seconds`): the median gap, so a
/// weekend or a holiday does not stretch it; 0 for fewer than two bars.
pub fn bar_seconds(bars: &Bars) -> f64 {
    let mut gaps: Vec<f64> = bars
        .times
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return 0.0;
    }
    gaps.sort_by(|a, b| cmp_f(*a, *b));
    gaps[gaps.len() / 2]
}

// ---------------------------------------------------------------------------
// Pivots
// ---------------------------------------------------------------------------

/// Bars whose value is the extreme of their own neighbourhood
/// (`_fractals`). Equal extremes count (a double top printed at one price
/// is two pivots), a plateau collapses to its first bar, and a flat
/// neighbourhood is no swing.
fn fractals(values: &[f64], left: usize, right: usize, kind: Side) -> Vec<usize> {
    let total = values.len();
    if total < left + right + 1 {
        return Vec::new();
    }
    let mut found: Vec<usize> = Vec::new();
    for centre in left..total - right {
        let value = values[centre];
        if !value.is_finite() {
            continue;
        }
        let neighbours: Vec<f64> = values[centre - left..centre]
            .iter()
            .chain(&values[centre + 1..centre + right + 1])
            .copied()
            .collect();
        if neighbours.is_empty() {
            continue;
        }
        let (most, least) = (max_of(&neighbours), min_of(&neighbours));
        let swing = match kind {
            Side::High => value >= most && value > least,
            Side::Low => value <= least && value < most,
        };
        if !swing {
            continue;
        }
        if found.last() == Some(&(centre - 1)) && values[centre - 1] == value {
            continue;
        }
        found.push(centre);
    }
    found
}

/// Prune pivots whose retracement is under `threshold` (`_zigzag`): a
/// same-side candidate replaces the running pivot when more extreme; an
/// opposite one is kept only when the move clears the threshold.
fn zigzag(candidates: Vec<Pivot>, threshold: f64) -> Vec<Pivot> {
    let mut kept: Vec<Pivot> = Vec::new();
    for candidate in candidates {
        let Some(last) = kept.last_mut() else {
            kept.push(candidate);
            continue;
        };
        if candidate.kind == last.kind {
            let more_extreme = match candidate.kind {
                Side::High => candidate.price > last.price,
                Side::Low => candidate.price < last.price,
            };
            if more_extreme {
                *last = candidate;
            }
        } else if (candidate.price - last.price).abs() >= threshold {
            kept.push(candidate);
        }
    }
    kept
}

/// Confirmed swing highs and lows inside `lo..=hi` (`swing_pivots`), at a
/// retracement filter of `significance` times the visible range (0 keeps
/// every fractal).
pub fn swing_pivots(
    bars: &Bars,
    lo: usize,
    hi: usize,
    significance: f64,
) -> (Vec<Pivot>, Vec<Pivot>) {
    let Some((first, last)) = bars.clip(lo, hi) else {
        return (Vec::new(), Vec::new());
    };
    let mut candidates: Vec<Pivot> = Vec::new();
    for (kind, values) in [(Side::High, &bars.highs), (Side::Low, &bars.lows)] {
        for index in fractals(values, DEFAULT_LEFT, DEFAULT_RIGHT, kind) {
            if (first..=last).contains(&index) {
                candidates.push(Pivot {
                    index,
                    time: bars.times[index],
                    price: values[index],
                    kind,
                });
            }
        }
    }
    candidates.sort_by_key(|p| (p.index, p.kind == Side::Low));
    let range = bars.span(first, last);
    if significance > 0.0 && range > 0.0 {
        candidates = zigzag(candidates, significance * range);
    }
    candidates.into_iter().partition(|p| p.kind == Side::High)
}

/// Swing points at the strictest rung of [`SIGNIFICANCE_LADDER`] that
/// leaves both sides [`MIN_VERTICES`] (`significant_pivots`), else the
/// loosest.
pub fn significant_pivots(bars: &Bars, lo: usize, hi: usize) -> (Vec<Pivot>, Vec<Pivot>) {
    let mut found = (Vec::new(), Vec::new());
    for threshold in SIGNIFICANCE_LADDER {
        found = swing_pivots(bars, lo, hi, threshold);
        if found.0.len() >= MIN_VERTICES && found.1.len() >= MIN_VERTICES {
            break;
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Levels
// ---------------------------------------------------------------------------

/// A horizontal level where swing points cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    /// The cluster's pivot nearest its mean: a price that printed.
    pub price: f64,
    pub touches: usize,
    /// `support` or `resistance` against the reference price.
    pub role: &'static str,
    /// Which pivots built it.
    pub side: Side,
    pub first_time: f64,
    pub last_time: f64,
}

/// Horizontal levels where swing points cluster (`support_resistance`),
/// highs and lows grouped separately by agglomeration on price, strongest
/// first (touches, then most recently touched). `reference` is the live
/// last price; `None` reads the role against the last close in the span.
pub fn support_resistance(
    bars: &Bars,
    lo: usize,
    hi: usize,
    reference: Option<f64>,
    min_touches: usize,
) -> Vec<Level> {
    let Some((first, last)) = bars.clip(lo, hi) else {
        return Vec::new();
    };
    let range = bars.span(first, last);
    if range <= 0.0 {
        return Vec::new();
    }
    let (highs, lows) = swing_pivots(bars, first, last, 0.0);
    let buckets = ((last - first + 1) / 8).clamp(12, 48);
    let tolerance = range / buckets as f64;
    let mark = reference
        .filter(|r| r.is_finite())
        .unwrap_or(bars.closes[last]);
    let mut levels: Vec<Level> = Vec::new();
    for (side, mut pivots) in [(Side::High, highs), (Side::Low, lows)] {
        pivots.sort_by(|a, b| cmp_f(a.price, b.price));
        let mut groups: Vec<Vec<Pivot>> = Vec::new();
        for p in pivots {
            match groups.last_mut() {
                Some(g) if p.price - g[0].price <= tolerance => g.push(p),
                _ => groups.push(vec![p]),
            }
        }
        for group in groups.iter().filter(|g| g.len() >= min_touches.max(1)) {
            levels.push(cluster_level(group, side, mark));
        }
    }
    levels.sort_by(|a, b| {
        b.touches
            .cmp(&a.touches)
            .then(cmp_f(b.last_time, a.last_time))
    });
    levels
}

fn cluster_level(group: &[Pivot], side: Side, mark: f64) -> Level {
    let mean = group.iter().map(|p| p.price).sum::<f64>() / group.len() as f64;
    let mut price = group[0].price;
    for p in &group[1..] {
        let nearer =
            cmp_f((p.price - mean).abs(), (price - mean).abs()).then(cmp_f(p.price, price));
        if nearer == Ordering::Less {
            price = p.price;
        }
    }
    Level {
        price: py_round(price, 4),
        touches: group.len(),
        role: if price >= mark {
            "resistance"
        } else {
            "support"
        },
        side,
        first_time: group.iter().map(|p| p.time).fold(f64::INFINITY, f64::min),
        last_time: group
            .iter()
            .map(|p| p.time)
            .fold(f64::NEG_INFINITY, f64::max),
    }
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

/// What a line must contain: nothing above a resistance rail, nothing
/// below a support rail, from its first anchor forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rail {
    Resistance,
    Support,
}

impl Rail {
    pub fn as_str(self) -> &'static str {
        match self {
            Rail::Resistance => "resistance",
            Rail::Support => "support",
        }
    }
}

/// A line through two pivots.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub from: Pivot,
    pub to: Pivot,
    /// Price per second.
    pub slope: f64,
    pub touches: usize,
    /// Over the touching pivots only; 1.0 below three.
    pub r2: f64,
    /// Bars that pierced it since its first anchor (0 when containment was
    /// not checked).
    pub breaks: usize,
}

/// The ranking of one candidate pair: fewest breaks, latest second anchor,
/// most touches, widest span, smallest residual.
#[derive(Debug, Clone, Copy)]
struct Score {
    breaks: usize,
    last_touch: f64,
    touches: usize,
    width: f64,
    error: f64,
}

impl Score {
    /// Python's `score > best` on the web's tuple
    /// `(-breaks, time, hits, width, -error)`.
    fn beats(&self, other: &Score) -> bool {
        other
            .breaks
            .cmp(&self.breaks)
            .then(cmp_f(self.last_touch, other.last_touch))
            .then(self.touches.cmp(&other.touches))
            .then(cmp_f(self.width, other.width))
            .then(cmp_f(-self.error, -other.error))
            == Ordering::Greater
    }
}

/// Bars from `since` on that pierced a line beyond the touch allowance.
fn breaks_of(
    bars: &Bars,
    rail: Rail,
    slope: f64,
    intercept: f64,
    since: f64,
    tolerance: f64,
) -> usize {
    bars.times
        .iter()
        .enumerate()
        .filter(|(_, t)| **t >= since)
        .filter(|(i, t)| {
            let value = slope * **t + intercept;
            let allowance = value.abs() * tolerance;
            match rail {
                Rail::Resistance => bars.highs[*i] > value + allowance,
                Rail::Support => bars.lows[*i] < value - allowance,
            }
        })
        .count()
}

/// The straight line through two of these pivots that price respected and
/// that touches the most of the rest (`fit_line`). With `contain`, breaks
/// decide first, which is what separates a trendline from a regression.
pub fn fit_line(
    pivots: &[Pivot],
    tolerance_pct: f64,
    contain: Option<(&Bars, Rail)>,
) -> Option<Fit> {
    let mut points: Vec<Pivot> = pivots
        .iter()
        .filter(|p| p.time.is_finite() && p.price.is_finite())
        .copied()
        .collect();
    if points.len() < 2 {
        return None;
    }
    points.sort_by(|a, b| cmp_f(a.time, b.time));
    if points.len() > MAX_TRENDLINE_PIVOTS {
        points.drain(..points.len() - MAX_TRENDLINE_PIVOTS);
    }
    let tolerance = tolerance_pct.max(0.0) / 100.0;
    let contain = contain.filter(|(bars, _)| !bars.is_empty());
    let mut best: Option<(Score, usize, usize)> = None;
    for i in 0..points.len() - 1 {
        for j in i + 1..points.len() {
            let width = points[j].time - points[i].time;
            if width <= 0.0 {
                continue;
            }
            let slope = (points[j].price - points[i].price) / width;
            let intercept = points[i].price - slope * points[i].time;
            let (touches, error) = touching(&points, slope, intercept, tolerance);
            let score = Score {
                breaks: contain.map_or(0, |(bars, rail)| {
                    breaks_of(bars, rail, slope, intercept, points[i].time, tolerance)
                }),
                last_touch: points[j].time,
                touches,
                width,
                error,
            };
            if best.as_ref().is_none_or(|(b, _, _)| score.beats(b)) {
                best = Some((score, i, j));
            }
        }
    }
    let (score, i, j) = best?;
    let (start, end) = (points[i], points[j]);
    let slope = (end.price - start.price) / (end.time - start.time);
    let intercept = start.price - slope * start.time;
    let (touches, r2) = touch_fit(&points, slope, intercept, tolerance);
    Some(Fit {
        from: start,
        to: end,
        slope,
        touches,
        r2: py_round(r2, 4),
        breaks: score.breaks,
    })
}

/// How many pivots sit on a line, and their summed squared residual.
fn touching(points: &[Pivot], slope: f64, intercept: f64, tolerance: f64) -> (usize, f64) {
    let mut hits = 0;
    let mut error = 0.0;
    for p in points {
        let value = slope * p.time + intercept;
        let residual = (p.price - value).abs();
        if residual <= value.abs() * tolerance {
            hits += 1;
            error += residual * residual;
        }
    }
    (hits, error)
}

/// The touches of the chosen line and its r2 over them (1.0 below three
/// touches or with no spread).
fn touch_fit(points: &[Pivot], slope: f64, intercept: f64, tolerance: f64) -> (usize, f64) {
    let on_line: Vec<(f64, f64)> = points
        .iter()
        .map(|p| (p.price, slope * p.time + intercept))
        .filter(|(price, value)| (price - value).abs() <= value.abs() * tolerance)
        .collect();
    if on_line.len() < 3 {
        return (on_line.len(), 1.0);
    }
    let mean = on_line.iter().map(|(p, _)| p).sum::<f64>() / on_line.len() as f64;
    let total: f64 = on_line.iter().map(|(p, _)| (p - mean).powi(2)).sum();
    let residual: f64 = on_line.iter().map(|(p, v)| (p - v).powi(2)).sum();
    let r2 = if total <= 0.0 {
        1.0
    } else {
        1.0 - residual / total
    };
    (on_line.len(), r2)
}

/// The swing sequence read as a trend (`structure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Structure {
    pub higher_highs: usize,
    pub lower_highs: usize,
    pub higher_lows: usize,
    pub lower_lows: usize,
    pub verdict: &'static str,
}

/// Count the transitions between consecutive swings and name the
/// structure (`structure`).
pub fn structure(highs: &[Pivot], lows: &[Pivot]) -> Structure {
    let steps = |pivots: &[Pivot]| -> (usize, usize) {
        let up = pivots
            .windows(2)
            .filter(|w| w[1].price > w[0].price)
            .count();
        let down = pivots
            .windows(2)
            .filter(|w| w[1].price < w[0].price)
            .count();
        (up, down)
    };
    let (hh, lh) = steps(highs);
    let (hl, ll) = steps(lows);
    let verdict = if lh > hh && ll > hl {
        "lower highs and lower lows, a downtrend"
    } else if hh > lh && hl > ll {
        "higher highs and higher lows, an uptrend"
    } else if lh > hh && hl > ll {
        "lower highs into higher lows, a contracting range"
    } else if hh > lh && ll > hl {
        "higher highs and lower lows, an expanding range"
    } else {
        "no clean sequence of highs and lows"
    };
    Structure {
        higher_highs: hh,
        lower_highs: lh,
        higher_lows: hl,
        lower_lows: ll,
        verdict,
    }
}
