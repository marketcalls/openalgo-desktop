//! Candlestick patterns on the latest visible bars, and chart patterns off
//! the swing sequence (the web's `chart_geometry.candlestick_patterns` and
//! `chart_patterns`).
//!
//! Every test is a proportion of a bar's own range, so one rule holds on a
//! 20 rupee stock and a 2,000 rupee one; single-bar reversals also read the
//! move into them, because a hammer after a rally is a hanging man.

use super::{Bars, Pivot};
use crate::agent::chart_contract::Anchor;
use crate::agent::tools::numbers::py_round;

/// Two prices within this fraction of the window's range are "equal".
const EQUAL_FRACTION: f64 = 0.03;

/// The move into a pattern bar is measured over this many bars.
const TREND_CONTEXT_BARS: usize = 5;

/// One candlestick pattern on one bar.
#[derive(Debug, Clone, PartialEq)]
pub struct CandleHit {
    pub name: &'static str,
    /// `bullish`, `bearish` or `neutral`.
    pub bias: &'static str,
    pub index: usize,
    pub time: f64,
    /// The extreme the marker belongs at: the high for a bearish pattern,
    /// the low otherwise.
    pub price: f64,
    /// Candles the pattern spans.
    pub bars: usize,
}

/// One bar's proportions.
#[derive(Debug, Clone, Copy)]
struct Candle {
    body: f64,
    range: f64,
    upper: f64,
    lower: f64,
    bullish: bool,
}

fn candle(bars: &Bars, i: usize) -> Candle {
    let (open, close) = (bars.opens[i], bars.closes[i]);
    Candle {
        body: (close - open).abs(),
        range: bars.highs[i] - bars.lows[i],
        upper: bars.highs[i] - open.max(close),
        lower: open.min(close) - bars.lows[i],
        bullish: close > open,
    }
}

/// The change in close over the bars before `i`; 0 without that history.
fn prior_move(bars: &Bars, i: usize) -> f64 {
    match i.checked_sub(TREND_CONTEXT_BARS) {
        Some(back) => bars.closes[i] - bars.closes[back],
        None => 0.0,
    }
}

struct Hits<'a> {
    bars: &'a Bars,
    found: Vec<CandleHit>,
}

impl Hits<'_> {
    fn record(&mut self, i: usize, name: &'static str, bias: &'static str, width: usize) {
        let price = if bias == "bearish" {
            self.bars.highs[i]
        } else {
            self.bars.lows[i]
        };
        self.found.push(CandleHit {
            name,
            bias,
            index: i,
            time: self.bars.times[i],
            price: py_round(price, 4),
            bars: width,
        });
    }

    fn bias(bullish: bool) -> &'static str {
        if bullish {
            "bullish"
        } else {
            "bearish"
        }
    }

    fn single(&mut self, i: usize, here: Candle) {
        let (body, span) = (here.body, here.range);
        let rising = prior_move(self.bars, i) > 0.0;
        if body <= 0.1 * span {
            self.record(i, "doji", "neutral", 1);
        } else if body >= 0.9 * span {
            self.record(i, "marubozu", Self::bias(here.bullish), 1);
        } else if body <= 0.3 * span && here.upper >= body && here.lower >= body {
            self.record(i, "spinning top", "neutral", 1);
        }
        if body > 0.0 && here.lower >= 2.0 * body && here.upper <= body {
            let name = if rising { "hanging man" } else { "hammer" };
            self.record(i, name, Self::bias(!rising), 1);
        }
        if body > 0.0 && here.upper >= 2.0 * body && here.lower <= body {
            let name = if rising {
                "shooting star"
            } else {
                "inverted hammer"
            };
            self.record(i, name, Self::bias(!rising), 1);
        }
    }

    fn pair(&mut self, i: usize, here: Candle, before: Candle) {
        let b = self.bars;
        let (open, close) = (b.opens[i], b.closes[i]);
        let (prev_open, prev_close) = (b.opens[i - 1], b.closes[i - 1]);
        let (prev_top, prev_bottom) = (prev_open.max(prev_close), prev_open.min(prev_close));
        let prev_mid = (prev_open + prev_close) / 2.0;
        if before.body > 0.0 && here.body > 0.0 {
            if !before.bullish && here.bullish && open <= prev_bottom && close >= prev_top {
                self.record(i, "bullish engulfing", "bullish", 2);
            } else if before.bullish && !here.bullish && open >= prev_top && close <= prev_bottom {
                self.record(i, "bearish engulfing", "bearish", 2);
            } else if !before.bullish
                && here.bullish
                && open < b.lows[i - 1]
                && prev_mid < close
                && close < prev_open
            {
                self.record(i, "piercing line", "bullish", 2);
            } else if before.bullish
                && !here.bullish
                && open > b.highs[i - 1]
                && prev_open < close
                && close < prev_mid
            {
                self.record(i, "dark cloud cover", "bearish", 2);
            } else if before.body >= 0.6 * before.range
                && here.body <= 0.5 * before.body
                && prev_bottom <= open.min(close)
                && open.max(close) <= prev_top
            {
                let name = if here.bullish {
                    "bullish harami"
                } else {
                    "bearish harami"
                };
                self.record(i, name, Self::bias(here.bullish), 2);
            }
        }
        let equal = EQUAL_FRACTION * here.range;
        if !before.bullish && here.bullish && (b.lows[i] - b.lows[i - 1]).abs() <= equal {
            self.record(i, "tweezer bottom", "bullish", 2);
        }
        if before.bullish && !here.bullish && (b.highs[i] - b.highs[i - 1]).abs() <= equal {
            self.record(i, "tweezer top", "bearish", 2);
        }
        if b.highs[i] <= b.highs[i - 1] && b.lows[i] >= b.lows[i - 1] {
            self.record(i, "inside bar", "neutral", 2);
        } else if b.highs[i] > b.highs[i - 1] && b.lows[i] < b.lows[i - 1] {
            self.record(i, "outside bar", "neutral", 2);
        }
    }

    fn triple(&mut self, i: usize, here: Candle, middle: Candle) {
        let b = self.bars;
        let first = candle(b, i - 2);
        let first_mid = (b.opens[i - 2] + b.closes[i - 2]) / 2.0;
        if first.body >= 0.5 * first.range
            && middle.body <= 0.4 * middle.range
            && here.body >= 0.5 * here.range
        {
            if !first.bullish && here.bullish && b.closes[i] > first_mid {
                self.record(i, "morning star", "bullish", 3);
            } else if first.bullish && !here.bullish && b.closes[i] < first_mid {
                self.record(i, "evening star", "bearish", 3);
            }
        }
        let (c0, c1, c2) = (b.closes[i - 2], b.closes[i - 1], b.closes[i]);
        if first.bullish && middle.bullish && here.bullish && c2 > c1 && c1 > c0 {
            self.record(i, "three white soldiers", "bullish", 3);
        } else if !(first.bullish || middle.bullish || here.bullish) && c2 < c1 && c1 < c0 {
            self.record(i, "three black crows", "bearish", 3);
        }
    }
}

/// Candlestick patterns on the last `scan` bars up to `hi`, not before
/// `lo` (`candlestick_patterns`), newest last. The scan ends at the
/// viewport's right edge, so a trader who scrolled back hears about the
/// bars they can see.
pub fn candlestick_patterns(bars: &Bars, scan: usize, lo: usize, hi: usize) -> Vec<CandleHit> {
    if bars.len() < 2 {
        return Vec::new();
    }
    let last = hi.min(bars.len() - 1);
    let start = (last + 1).saturating_sub(scan.max(1)).max(lo).max(1);
    let mut hits = Hits {
        bars,
        found: Vec::new(),
    };
    for i in start..=last {
        let here = candle(bars, i);
        if here.range <= 0.0 {
            continue;
        }
        let before = candle(bars, i - 1);
        hits.single(i, here);
        hits.pair(i, here, before);
        if i >= 2 {
            hits.triple(i, here, before);
        }
    }
    hits.found
}

/// One chart pattern: its anchors in the order the web lists them, and
/// the segments that draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartPattern {
    pub name: &'static str,
    pub bias: &'static str,
    pub anchors: Vec<Anchor>,
    /// Pairs of indices into `anchors`, one per segment to draw. A double
    /// top or a head and shoulders joins its anchors in turn; a triangle or
    /// a rectangle is two rails, upper (`[0, 1]`) and lower (`[2, 3]`), and
    /// never a segment from the upper rail back to the lower one (D-029:
    /// the web joins every consecutive anchor and so draws a false diagonal
    /// from the second high to the first low).
    pub segments: Vec<[usize; 2]>,
}

impl ChartPattern {
    fn joined(name: &'static str, bias: &'static str, pivots: &[Pivot]) -> ChartPattern {
        ChartPattern {
            name,
            bias,
            anchors: pivots.iter().map(Pivot::anchor).collect(),
            segments: (1..pivots.len()).map(|i| [i - 1, i]).collect(),
        }
    }

    fn rails(
        name: &'static str,
        bias: &'static str,
        upper: [Pivot; 2],
        lower: [Pivot; 2],
    ) -> ChartPattern {
        ChartPattern {
            name,
            bias,
            anchors: upper.iter().chain(&lower).map(Pivot::anchor).collect(),
            segments: vec![[0, 1], [2, 3]],
        }
    }
}

/// The pivots strictly between two others in time.
fn between(pivots: &[Pivot], a: &Pivot, b: &Pivot) -> Vec<Pivot> {
    pivots
        .iter()
        .filter(|p| a.time < p.time && p.time < b.time)
        .copied()
        .collect()
}

/// The first pivot with the lowest (or highest) price, as Python's
/// `min`/`max` with a key pick it.
fn extreme(pivots: &[Pivot], lowest: bool) -> Option<Pivot> {
    let mut best: Option<Pivot> = None;
    for p in pivots {
        let wins = best.is_none_or(|b| {
            if lowest {
                p.price < b.price
            } else {
                p.price > b.price
            }
        });
        if wins {
            best = Some(*p);
        }
    }
    best
}

/// A double top, or with `bottom` a double bottom: the last two swings of a
/// side level, with a swing of the other side between them.
fn double(highs: &[Pivot], lows: &[Pivot], equal: f64, bottom: bool) -> Option<ChartPattern> {
    let (side, other) = if bottom { (lows, highs) } else { (highs, lows) };
    let [.., a, b] = side else {
        return None;
    };
    let middle = extreme(&between(other, a, b), !bottom)?;
    if (a.price - b.price).abs() > equal {
        return None;
    }
    Some(if bottom {
        ChartPattern::joined("double bottom", "bullish", &[*a, middle, *b])
    } else {
        ChartPattern::joined("double top", "bearish", &[*a, middle, *b])
    })
}

/// A head and shoulders, or with `inverse` its mirror: three swings of a
/// side with the middle the extreme, level shoulders and two neckline
/// swings between them.
fn head_and_shoulders(
    highs: &[Pivot],
    lows: &[Pivot],
    equal: f64,
    inverse: bool,
) -> Option<ChartPattern> {
    let (side, other) = if inverse {
        (lows, highs)
    } else {
        (highs, lows)
    };
    let [.., left, head, right] = side else {
        return None;
    };
    if other.len() < 2 {
        return None;
    }
    let neck = between(other, left, right);
    let head_out = if inverse {
        head.price < left.price && head.price < right.price
    } else {
        head.price > left.price && head.price > right.price
    };
    let (Some(n0), Some(n1)) = (neck.first(), neck.last()) else {
        return None;
    };
    if !head_out || (left.price - right.price).abs() > equal || neck.len() < 2 {
        return None;
    }
    let pivots = [*left, *n0, *head, *n1, *right];
    Some(if inverse {
        ChartPattern::joined("inverse head and shoulders", "bullish", &pivots)
    } else {
        ChartPattern::joined("head and shoulders", "bearish", &pivots)
    })
}

/// A triangle or a rectangle from the last two swings of each side.
fn converging(highs: &[Pivot], lows: &[Pivot], equal: f64) -> Option<ChartPattern> {
    let ([.., h0, h1], [.., l0, l1]) = (highs, lows) else {
        return None;
    };
    let high_step = h1.price - h0.price;
    let low_step = l1.price - l0.price;
    let flat_highs = high_step.abs() <= equal;
    let flat_lows = low_step.abs() <= equal;
    let (name, bias) = if flat_highs && low_step > equal {
        ("ascending triangle", "bullish")
    } else if flat_lows && high_step < -equal {
        ("descending triangle", "bearish")
    } else if high_step < -equal && low_step > equal {
        ("symmetrical triangle", "neutral")
    } else if flat_highs && flat_lows {
        ("rectangle range", "neutral")
    } else {
        return None;
    };
    Some(ChartPattern::rails(name, bias, [*h0, *h1], [*l0, *l1]))
}

/// Chart patterns read off the swing sequence (`chart_patterns`), with
/// "equal" measured as a share of the same span the pivots came from.
pub fn chart_patterns(
    bars: &Bars,
    highs: &[Pivot],
    lows: &[Pivot],
    lo: usize,
    hi: usize,
) -> Vec<ChartPattern> {
    let Some((first, last)) = bars.clip(lo, hi) else {
        return Vec::new();
    };
    let window = bars.span(first, last);
    if window <= 0.0 {
        return Vec::new();
    }
    let equal = EQUAL_FRACTION * window;
    [
        double(highs, lows, equal, false),
        double(highs, lows, equal, true),
        head_and_shoulders(highs, lows, equal, false),
        head_and_shoulders(highs, lows, equal, true),
        converging(highs, lows, equal),
    ]
    .into_iter()
    .flatten()
    .collect()
}
