//! Geometry from real bars (the web's `test/test_agent_chart_surface.py`:
//! `TestGeometryReadsRealBars`, `TestARailIsALinePriceRespected`,
//! `TestAConsolidationIsTheWholeStretch`), on the same hand-built bars.

use super::zones::RANGE_BAND_FRACTION;
use super::*;

pub(crate) const DAY: f64 = 86_400.0;
pub(crate) const START: f64 = 1_700_000_000.0;

/// One daily bar per close, opened at the close, `spread` either side
/// (`bars_from`).
pub(crate) fn bars_from(closes: &[f64], spread: f64) -> Bars {
    Bars {
        times: (0..closes.len()).map(|i| START + i as f64 * DAY).collect(),
        opens: closes.to_vec(),
        highs: closes.iter().map(|c| c + spread).collect(),
        lows: closes.iter().map(|c| c - spread).collect(),
        closes: closes.to_vec(),
    }
}

/// A close series walking between turning points, `leg` bars per leg
/// (`zigzag_closes`).
pub(crate) fn zigzag_closes(levels: &[f64], leg: usize) -> Vec<f64> {
    let mut out = vec![levels[0]];
    for pair in levels.windows(2) {
        for step in 1..=leg {
            out.push(pair[0] + (pair[1] - pair[0]) * step as f64 / leg as f64);
        }
    }
    out
}

fn last(bars: &Bars) -> usize {
    bars.len() - 1
}

// -- levels, rails, structure and zones read real bars -------------------------

#[test]
fn a_level_is_a_price_that_actually_printed() {
    let bars = bars_from(
        &zigzag_closes(&[100., 130., 100., 130., 100., 130., 115.], 8),
        4.0,
    );
    let levels = support_resistance(&bars, 0, last(&bars), Some(115.0), 2);
    assert!(!levels.is_empty());
    let (low, high) = (min_of(&bars.lows), max_of(&bars.highs));
    for l in &levels {
        assert!(l.touches >= 2);
        assert!(low <= l.price && l.price <= high);
        assert!(["support", "resistance"].contains(&l.role));
        assert!(bars.times.contains(&l.first_time));
    }
}

#[test]
fn support_and_resistance_are_named_against_the_live_price() {
    let bars = bars_from(
        &zigzag_closes(&[100., 130., 100., 130., 100., 130., 115.], 8),
        4.0,
    );
    let roles = |reference: f64| -> Vec<&'static str> {
        let mut r: Vec<&str> = support_resistance(&bars, 0, last(&bars), Some(reference), 2)
            .iter()
            .map(|l| l.role)
            .collect();
        r.dedup();
        r
    };
    assert_eq!(roles(145.0), ["support"]);
    assert_eq!(roles(85.0), ["resistance"]);
}

#[test]
fn a_fitted_rail_runs_through_swings_that_printed() {
    let bars = bars_from(
        &zigzag_closes(&[100., 140., 110., 150., 120., 160., 130.], 8),
        4.0,
    );
    let (highs, _lows) = significant_pivots(&bars, 0, last(&bars));
    let fit = fit_line(&highs, TOUCH_TOLERANCE_PCT, None).unwrap_or_else(|| panic!("no fit"));
    assert!(bars.times.contains(&fit.from.time) && bars.times.contains(&fit.to.time));
    assert!(fit.touches >= 2);
    assert!((0.0..=1.0).contains(&fit.r2));
    assert!(fit.slope > 0.0);
}

#[test]
fn structure_reads_a_downtrend_as_one() {
    let closes = zigzag_closes(
        &[120., 170., 135., 160., 125., 150., 115., 140., 105., 125.],
        8,
    );
    let bars = bars_from(&closes, 4.0);
    let (highs, lows) = significant_pivots(&bars, 0, last(&bars));
    let s = structure(&highs, &lows);
    assert!(s.lower_highs >= 2 && s.lower_lows >= 2);
    assert_eq!((s.higher_highs, s.higher_lows), (0, 0));
    assert!(s.verdict.contains("down"), "{}", s.verdict);
}

#[test]
fn both_edges_of_a_zone_printed() {
    let bars = bars_from(&zigzag_closes(&[140., 100., 145., 105., 150.], 8), 4.0);
    let band = zone(&bars, ZoneKind::Demand, 0, last(&bars)).unwrap_or_else(|| panic!("no zone"));
    assert!(band.low >= min_of(&bars.lows) && band.high <= max_of(&bars.highs));
    assert!(band.low < band.high);
    assert!(bars.times.contains(&band.pivot_time));
}

#[test]
fn the_viewport_clips_the_window() {
    let bars = bars_from(&[100.0; 50], 1.0);
    assert_eq!(
        bars.window(Some(bars.times[10]), Some(bars.times[20])),
        (10, 20)
    );
}

#[test]
fn a_viewport_off_the_loaded_data_answers_about_everything_rather_than_nothing() {
    let bars = bars_from(&[100.0; 50], 1.0);
    let after = bars.times[49] + 10.0 * DAY;
    assert_eq!(bars.window(Some(after), Some(after + DAY)), (0, 49));
    assert_eq!(bars.window(None, None), (0, 49));
    assert_eq!(bars.window(Some(f64::NAN), None), (0, 49));
}

#[test]
fn bar_seconds_is_the_median_so_a_holiday_does_not_stretch_it() {
    let mut bars = bars_from(&[100.0; 20], 1.0);
    for t in bars.times.iter_mut().skip(11) {
        *t += 30.0 * DAY;
    }
    assert_eq!(bar_seconds(&bars), DAY);
    assert_eq!(bar_seconds(&bars_from(&[1.0], 1.0)), 0.0);
}

// -- a rail is a line price respected -------------------------------------------

/// A rally into three lower highs, then a decline to a much lower high: the
/// line touching the most swings is one price spends the rally above.
const LOWER_HIGHS: [f64; 10] = [80., 70., 121., 100., 119., 98., 117., 60., 91., 55.];

fn rail_bars() -> Bars {
    bars_from(&zigzag_closes(&LOWER_HIGHS, 6), 1.0)
}

/// Bars above a line from its first anchor on, counted here rather than
/// read off the fit (`breaks` is 0 whenever containment was not asked for).
fn pierced(bars: &Bars, fit: &Fit) -> usize {
    let intercept = fit.from.price - fit.slope * fit.from.time;
    bars.times
        .iter()
        .enumerate()
        .filter(|(i, t)| **t >= fit.from.time && bars.highs[*i] > fit.slope * **t + intercept)
        .count()
}

#[test]
fn ranking_on_touches_alone_draws_a_line_price_lives_above() {
    let bars = rail_bars();
    let (highs, _) = significant_pivots(&bars, 0, last(&bars));
    let loose = fit_line(&highs, TOUCH_TOLERANCE_PCT, None).unwrap_or_else(|| panic!("no fit"));
    assert!(
        pierced(&bars, &loose) > 5,
        "this window has to punish a touch-only fit"
    );
}

#[test]
fn a_contained_rail_has_nothing_above_it_and_differs_from_the_loose_fit() {
    let bars = rail_bars();
    let (highs, _) = significant_pivots(&bars, 0, last(&bars));
    let held = fit_line(&highs, TOUCH_TOLERANCE_PCT, Some((&bars, Rail::Resistance)))
        .unwrap_or_else(|| panic!("no fit"));
    assert_eq!(pierced(&bars, &held), 0);
    assert_eq!(held.breaks, 0);
    let loose = fit_line(&highs, TOUCH_TOLERANCE_PCT, None).unwrap_or_else(|| panic!("no fit"));
    assert_ne!(
        (held.from.index, held.to.index),
        (loose.from.index, loose.to.index)
    );
}

// -- a consolidation is the whole sideways stretch -------------------------------

#[test]
fn the_band_holds_the_wick_that_printed_inside_the_range() {
    let mut closes: Vec<f64> = (0..20).map(|n| 130.0 + n as f64).collect();
    closes.extend((0..68).map(|n| if n % 2 == 1 { 109.0 } else { 101.0 }));
    let mut bars = bars_from(&closes, 1.0);
    bars.lows[30] = 96.0;
    let band =
        consolidation(&bars, 0, last(&bars), MIN_RANGE_BARS).unwrap_or_else(|| panic!("none"));
    assert_eq!(band.low, 96.0);
    assert_eq!(band.high, 110.0);
    assert!(band.bars >= 60, "{}", band.bars);
    assert!(bars.lows.contains(&band.low) && bars.highs.contains(&band.high));
}

#[test]
fn a_trending_leg_does_not_qualify_just_because_it_fits_the_band() {
    let closes: Vec<f64> = (0..80).map(|n| 100.0 + 2.0 * n as f64).collect();
    let bars = bars_from(&closes, 2.0);
    let visible = max_of(&bars.highs) - min_of(&bars.lows);
    let n = bars.len();
    let widest = (0..n)
        .flat_map(|s| (s..n).map(move |e| (s, e)))
        .filter(|(s, e)| {
            max_of(&bars.highs[*s..=*e]) - min_of(&bars.lows[*s..=*e])
                <= RANGE_BAND_FRACTION * visible
        })
        .map(|(s, e)| e - s + 1)
        .max()
        .unwrap_or(0);
    assert!(widest >= 12, "not vacuous");
    assert_eq!(consolidation(&bars, 0, last(&bars), MIN_RANGE_BARS), None);
}

#[test]
fn narrow_is_measured_against_what_is_on_screen() {
    let closes: Vec<f64> = (0..40)
        .map(|n| if n % 2 == 1 { 104.0 } else { 96.0 })
        .collect();
    let bars = bars_from(&closes, 1.0);
    assert_eq!(consolidation(&bars, 0, last(&bars), MIN_RANGE_BARS), None);
}

// -- patterns ----------------------------------------------------------------------

#[test]
fn a_bullish_engulfing_is_found_on_its_bar_at_its_low() {
    let mut bars = bars_from(&[100.0; 12], 2.0);
    // A falling bar, then one that opens below its body and closes above it.
    (bars.opens[10], bars.closes[10]) = (101.0, 99.0);
    (bars.opens[11], bars.closes[11]) = (98.5, 102.0);
    bars.lows[11] = 97.0;
    let hits = candlestick_patterns(&bars, 30, 0, 11);
    let hit = hits
        .iter()
        .find(|h| h.name == "bullish engulfing")
        .unwrap_or_else(|| panic!("{:?}", hits));
    assert_eq!(
        (hit.index, hit.bias, hit.price, hit.bars),
        (11, "bullish", 97.0, 2)
    );
}

#[test]
fn the_scan_ends_at_the_viewport_and_spans_the_last_bars() {
    let bars = bars_from(&zigzag_closes(&[100., 130., 100., 130.], 8), 3.0);
    let hits = candlestick_patterns(&bars, 5, 0, 20);
    assert!(hits.iter().all(|h| (16..=20).contains(&h.index)));
    assert!(candlestick_patterns(&bars_from(&[1.0], 1.0), 5, 0, 0).is_empty());
}

fn pivot(index: usize, price: f64, kind: Side) -> Pivot {
    Pivot {
        index,
        time: START + index as f64 * DAY,
        price,
        kind,
    }
}

#[test]
fn a_double_top_joins_its_peaks_through_the_trough() {
    let bars = bars_from(&[100.0; 30], 20.0);
    let highs = [pivot(5, 120.0, Side::High), pivot(20, 120.5, Side::High)];
    let lows = [pivot(10, 95.0, Side::Low), pivot(12, 90.0, Side::Low)];
    let found = chart_patterns(&bars, &highs, &lows, 0, 29);
    let top = found
        .iter()
        .find(|p| p.name == "double top")
        .unwrap_or_else(|| panic!("{:?}", found));
    assert_eq!(top.bias, "bearish");
    let prices: Vec<f64> = top.anchors.iter().map(|a| a.price).collect();
    assert_eq!(prices, [120.0, 90.0, 120.5]);
    assert_eq!(top.segments, [[0, 1], [1, 2]]);
}

/// D-029: a triangle is two rails. The web joins every consecutive anchor
/// of `[high-2, high-1, low-2, low-1]`, so one segment ran from the second
/// high back to the first low, a diagonal that is no part of the pattern.
#[test]
fn a_triangle_is_two_rails_and_never_a_diagonal_between_them() {
    let bars = bars_from(&[100.0; 30], 20.0);
    let highs = [pivot(4, 110.0, Side::High), pivot(16, 110.2, Side::High)];
    let lows = [pivot(8, 88.0, Side::Low), pivot(20, 98.0, Side::Low)];
    let found = chart_patterns(&bars, &highs, &lows, 0, 29);
    let triangle = found
        .iter()
        .find(|p| p.name == "ascending triangle")
        .unwrap_or_else(|| panic!("{:?}", found));
    assert_eq!(triangle.anchors.len(), 4, "the web's anchor list is kept");
    assert_eq!(triangle.segments, [[0, 1], [2, 3]]);
    let rectangle = chart_patterns(
        &bars,
        &highs,
        &[pivot(8, 88.0, Side::Low), pivot(20, 88.5, Side::Low)],
        0,
        29,
    );
    let range = rectangle
        .iter()
        .find(|p| p.name == "rectangle range")
        .unwrap_or_else(|| panic!("{:?}", rectangle));
    assert_eq!(range.segments, [[0, 1], [2, 3]]);
}
