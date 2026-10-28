//! Demand and supply bands off a real swing, and consolidations (the web's
//! `chart_geometry.zone` and `consolidation`).

use super::{max_of, min_of, significant_pivots, Bars};
use crate::agent::tools::numbers::py_round;

/// How tall a band may be and still be called a range, as a share of the
/// whole visible move: room for the wick that printed inside the range
/// (measured by the web on RELIANCE NSE daily, 2026-04-01 to 2026-09-04).
pub const RANGE_BAND_FRACTION: f64 = 0.45;

/// How far price may travel between a range's two ends, as a share of its
/// own band: what stops a tall box calling a trending leg a range
/// (measured by the web on TITAN NSE daily, 2026-06-01 to 2026-09-04).
pub const RANGE_DRIFT_FRACTION: f64 = 0.5;

/// Shortest run worth calling a consolidation.
pub const MIN_RANGE_BARS: usize = 12;

/// Which band to derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneKind {
    /// The base under price, off the latest significant swing low.
    Demand,
    /// The ceiling above it, off the latest significant swing high.
    Supply,
}

impl ZoneKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ZoneKind::Demand => "demand",
            ZoneKind::Supply => "supply",
        }
    }
}

/// A band whose edges both printed.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneBand {
    pub low: f64,
    pub high: f64,
    pub from_time: f64,
    pub to_time: f64,
    pub pivot_time: f64,
    pub pivot_price: f64,
    pub bars_since: usize,
}

/// A demand or supply band from the three bars around the latest
/// significant swing of that side (`zone`): demand runs from their lowest
/// low to their highest body top, supply from the lowest body bottom to
/// the highest high. `None` with no such swing in the span.
pub fn zone(bars: &Bars, kind: ZoneKind, lo: usize, hi: usize) -> Option<ZoneBand> {
    let (first, last) = bars.clip(lo, hi)?;
    let (highs, lows) = significant_pivots(bars, first, last);
    let anchor = *match kind {
        ZoneKind::Demand => lows.last(),
        ZoneKind::Supply => highs.last(),
    }?;
    let left = anchor.index.saturating_sub(1);
    let right = (anchor.index + 1).min(bars.len() - 1);
    let tops: Vec<f64> = (left..=right)
        .map(|i| bars.opens[i].max(bars.closes[i]))
        .collect();
    let bottoms: Vec<f64> = (left..=right)
        .map(|i| bars.opens[i].min(bars.closes[i]))
        .collect();
    let (band_low, band_high) = match kind {
        ZoneKind::Demand => (min_of(&bars.lows[left..=right]), max_of(&tops)),
        ZoneKind::Supply => (min_of(&bottoms), max_of(&bars.highs[left..=right])),
    };
    if band_high <= band_low {
        return None;
    }
    Some(ZoneBand {
        low: py_round(band_low, 4),
        high: py_round(band_high, 4),
        from_time: bars.times[left],
        to_time: bars.times[last],
        pivot_time: anchor.time,
        pivot_price: py_round(anchor.price, 4),
        bars_since: last - anchor.index,
    })
}

/// The widest stretch that stayed in one narrow band.
#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub low: f64,
    pub high: f64,
    pub from_time: f64,
    pub to_time: f64,
    pub bars: usize,
    /// Band height over its low, in percent (2 places).
    pub width_pct: f64,
    /// Net progress between the run's ends over its band, in percent (1
    /// place): the trend test this run passed.
    pub drift_pct: f64,
    /// Bars whose high or low came within 15% of the band of an edge.
    pub touches: usize,
}

/// The best run so far: longest, then tighter.
#[derive(Debug, Clone, Copy)]
struct Run {
    length: usize,
    width: f64,
    start: usize,
    end: usize,
    drift: f64,
}

/// The widest run inside `lo..=hi` whose band stays within
/// [`RANGE_BAND_FRACTION`] of the visible move and whose closes drift no
/// more than [`RANGE_DRIFT_FRACTION`] of that band (`consolidation`).
pub fn consolidation(bars: &Bars, lo: usize, hi: usize, min_bars: usize) -> Option<Range> {
    let (first, last) = bars.clip(lo, hi)?;
    if last - first + 1 < min_bars {
        return None;
    }
    let visible = bars.span(first, last);
    if visible <= 0.0 {
        return None;
    }
    let best = widest_run(bars, first, last, min_bars, visible * RANGE_BAND_FRACTION)?;
    let band_high = max_of(&bars.highs[best.start..=best.end]);
    let band_low = min_of(&bars.lows[best.start..=best.end]);
    if band_high <= band_low {
        return None;
    }
    let edge = (band_high - band_low) * 0.15;
    let touches = (best.start..=best.end)
        .filter(|i| bars.highs[*i] >= band_high - edge || bars.lows[*i] <= band_low + edge)
        .count();
    Some(Range {
        low: py_round(band_low, 4),
        high: py_round(band_high, 4),
        from_time: bars.times[best.start],
        to_time: bars.times[best.end],
        bars: best.length,
        width_pct: py_round((band_high - band_low) / band_low * 100.0, 2),
        drift_pct: py_round(best.drift / (band_high - band_low) * 100.0, 1),
        touches,
    })
}

/// For every start bar, extend while the running band fits `tolerance`,
/// keeping the longest qualifying run (ties to the tighter band).
fn widest_run(
    bars: &Bars,
    first: usize,
    last: usize,
    min_bars: usize,
    tolerance: f64,
) -> Option<Run> {
    let mut best: Option<Run> = None;
    for start in first..last + 2 - min_bars {
        let mut band_high = bars.highs[start];
        let mut band_low = bars.lows[start];
        for end in start..=last {
            band_high = band_high.max(bars.highs[end]);
            band_low = band_low.min(bars.lows[end]);
            if band_high - band_low > tolerance {
                break;
            }
            let length = end - start + 1;
            if length < min_bars {
                continue;
            }
            let width = band_high - band_low;
            let drift = (bars.closes[end] - bars.closes[start]).abs();
            if width <= 0.0 || drift > RANGE_DRIFT_FRACTION * width {
                continue;
            }
            let better =
                best.is_none_or(|b| length > b.length || (length == b.length && width < b.width));
            if better {
                best = Some(Run {
                    length,
                    width,
                    start,
                    end,
                    drift,
                });
            }
        }
    }
    best
}
