//! The `openalgo.ta` indicators the MCP research tools use, as pure
//! functions over `f64` slices (`NaN` marks a value that does not exist yet).
//!
//! Each kernel follows the NumPy reference in the web's SDK,
//! `openalgo/indicators/_backend.py` (the documented equivalent of its
//! compiled core), and each wrapper check follows the indicator classes
//! (`base.py:validate_period`, `trend.py`, `momentum.py`, `volatility.py`,
//! `hybrid.py`, `statistics.py`, `utils.py`). Line numbers refer to
//! `openalgo` 2.0.5 as installed with the web.
//!
//! Errors carry the Python exception type and message the web would report,
//! since the tools put them in their output.

use serde_json::{Map, Value};

/// A Python exception: type name and message.
#[derive(Debug, Clone, PartialEq)]
pub struct PyErr {
    pub kind: &'static str,
    pub message: String,
}

impl PyErr {
    pub fn value(msg: impl Into<String>) -> Self {
        Self {
            kind: "ValueError",
            message: msg.into(),
        }
    }
    pub fn type_err(msg: impl Into<String>) -> Self {
        Self {
            kind: "TypeError",
            message: msg.into(),
        }
    }
}

pub type R<T> = Result<T, PyErr>;

const NAN: f64 = f64::NAN;

/// `base.py:112` `validate_period`.
pub fn validate_period(period: i64, len: usize) -> R<()> {
    if period <= 0 {
        return Err(PyErr::value(format!(
            "Period must be positive, got {}",
            period
        )));
    }
    if period as usize > len {
        return Err(PyErr::value(format!(
            "Period ({}) cannot be greater than data length ({})",
            period, len
        )));
    }
    Ok(())
}

/// `base.py:24` `validate_input`: empty input is refused.
fn non_empty(d: &[f64]) -> R<()> {
    if d.is_empty() {
        Err(PyErr::value("Input data cannot be empty"))
    } else {
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Kernels (`_backend.py`)
// ----------------------------------------------------------------------

/// Rolling reduction over complete windows; `NaN` when the window holds one
/// (`_backend.py:1919` `_roll` with `np.max` / `np.min` / `np.mean`, and the
/// window-local NaN rule of `_roll_nan`, `_backend.py:51`).
fn roll(data: &[f64], period: usize, f: impl Fn(&[f64]) -> f64) -> Vec<f64> {
    let n = data.len();
    let mut out = vec![NAN; n];
    if period == 0 || n < period {
        return out;
    }
    for i in period - 1..n {
        let w = &data[i + 1 - period..=i];
        out[i] = if w.iter().any(|x| x.is_nan()) {
            NAN
        } else {
            f(w)
        };
    }
    out
}

fn mean(w: &[f64]) -> f64 {
    w.iter().sum::<f64>() / w.len() as f64
}

/// `_backend.py:75` `sma`.
pub fn sma_k(data: &[f64], period: usize) -> Vec<f64> {
    roll(data, period, mean)
}

/// `_backend.py:89` `wma`: weights 1..=period, newest heaviest.
pub fn wma_k(data: &[f64], period: usize) -> Vec<f64> {
    let wsum = (period * (period + 1) / 2) as f64;
    roll(data, period, |w| {
        w.iter()
            .enumerate()
            .map(|(j, x)| x * (j + 1) as f64)
            .sum::<f64>()
            / wsum
    })
}

/// `_backend.py:128` `ema`: seeded at the first finite value, NaNs skipped
/// (left NaN in the output) without breaking the recursion.
pub fn ema_k(data: &[f64], period: usize) -> Vec<f64> {
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut out = vec![NAN; data.len()];
    let mut prev = NAN;
    for (i, &x) in data.iter().enumerate() {
        if x.is_nan() {
            continue;
        }
        prev = if prev.is_nan() {
            x
        } else {
            alpha * x + (1.0 - alpha) * prev
        };
        out[i] = prev;
    }
    out
}

/// `_backend.py:35` `_first_clean_window`.
fn first_clean_window(data: &[f64], period: usize) -> Option<usize> {
    if period == 0 || data.len() < period {
        return None;
    }
    let mut run = 0;
    for (i, x) in data.iter().enumerate() {
        run = if x.is_nan() { 0 } else { run + 1 };
        if run == period {
            return Some(i + 1 - period);
        }
    }
    None
}

/// `_backend.py:406` `ema_wilder` (RMA): seeded with the mean of the first
/// NaN-free window, then `(prev * (p - 1) + x) / p`; a NaN repeats `prev`.
pub fn ema_wilder_k(data: &[f64], period: usize) -> Vec<f64> {
    let n = data.len();
    let mut out = vec![NAN; n];
    let Some(fv) = first_clean_window(data, period) else {
        return out;
    };
    let start = fv + period - 1;
    out[start] = data[fv..fv + period].iter().sum::<f64>() / period as f64;
    for i in start + 1..n {
        let v = data[i];
        out[i] = if v.is_nan() {
            out[i - 1]
        } else {
            (out[i - 1] * (period as f64 - 1.0) + v) / period as f64
        };
    }
    out
}

/// `_backend.py:1928` `ema_sma`: EMA seeded with the SMA of the first window.
pub fn ema_sma_k(data: &[f64], period: usize) -> Vec<f64> {
    let n = data.len();
    let mut out = vec![NAN; n];
    let Some(fv) = first_clean_window(data, period) else {
        return out;
    };
    let alpha = 2.0 / (period as f64 + 1.0);
    let start = fv + period - 1;
    let mut prev = data[fv..fv + period].iter().sum::<f64>() / period as f64;
    out[start] = prev;
    for i in start + 1..n {
        if data[i].is_nan() {
            continue;
        }
        prev = alpha * data[i] + (1.0 - alpha) * prev;
        out[i] = prev;
    }
    out
}

/// Population standard deviation per window (`_backend.py:148` `stdev`,
/// `_backend.py:1828` `_win_std`).
pub fn stdev_k(data: &[f64], period: usize) -> Vec<f64> {
    roll(data, period, |w| {
        let m = mean(w);
        (w.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / w.len() as f64)
            .max(0.0)
            .sqrt()
    })
}

/// `_backend.py:164` `true_range`.
pub fn true_range_k(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let n = high.len();
    let mut tr = vec![NAN; n];
    if n == 0 {
        return tr;
    }
    tr[0] = high[0] - low[0];
    for i in 1..n {
        let hl = high[i] - low[i];
        let hc = (high[i] - close[i - 1]).abs();
        let lc = (low[i] - close[i - 1]).abs();
        tr[i] = if hl.is_nan() || hc.is_nan() || lc.is_nan() {
            NAN
        } else {
            hl.max(hc).max(lc)
        };
    }
    tr
}

/// `_backend.py:186` `atr_wilder`.
pub fn atr_k(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    if period == 0 || high.len() < period {
        return vec![NAN; high.len()];
    }
    ema_wilder_k(&true_range_k(high, low, close), period)
}

/// `_backend.py:201` `rsi` (Wilder averages seeded on the first clean
/// `period + 1` values).
pub fn rsi_k(data: &[f64], period: usize) -> Vec<f64> {
    let n = data.len();
    let mut out = vec![NAN; n];
    if period == 0 || n < period + 1 {
        return out;
    }
    let Some(fv) = first_clean_window(data, period + 1) else {
        return out;
    };
    let deltas: Vec<f64> = data.windows(2).map(|w| w[1] - w[0]).collect();
    let gain = |d: f64| if d > 0.0 { d } else { 0.0 };
    let loss = |d: f64| if d < 0.0 { -d } else { 0.0 };
    let p = period as f64;
    let mut ag = deltas[fv..fv + period]
        .iter()
        .map(|&d| gain(d))
        .sum::<f64>()
        / p;
    let mut al = deltas[fv..fv + period]
        .iter()
        .map(|&d| loss(d))
        .sum::<f64>()
        / p;
    let value = |ag: f64, al: f64| {
        if al == 0.0 {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + ag / al)
        }
    };
    let start = fv + period;
    out[start] = value(ag, al);
    for i in start..n - 1 {
        let d = deltas[i];
        if d.is_nan() {
            continue;
        }
        ag = (ag * (p - 1.0) + gain(d)) / p;
        al = (al * (p - 1.0) + loss(d)) / p;
        out[i + 1] = value(ag, al);
    }
    out
}

/// `_backend.py:267` `cci`.
pub fn cci_k(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    let mut out = vec![NAN; n];
    if period == 0 || n < period {
        return out;
    }
    let tp: Vec<f64> = (0..n)
        .map(|i| (high[i] + low[i] + close[i]) / 3.0)
        .collect();
    let p = period as f64;
    let mut rsum: f64 = tp[..period].iter().sum();
    for i in period - 1..n {
        if i > period - 1 {
            rsum = rsum + tp[i] - tp[i - period];
        }
        let m = rsum / p;
        let md = tp[i + 1 - period..=i]
            .iter()
            .map(|x| (x - m).abs())
            .sum::<f64>()
            / p;
        out[i] = if md != 0.0 {
            (tp[i] - m) / (0.015 * md)
        } else {
            0.0
        };
    }
    out
}

fn max_of(w: &[f64]) -> f64 {
    w.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
}
fn min_of(w: &[f64]) -> f64 {
    w.iter().cloned().fold(f64::INFINITY, f64::min)
}

/// Rolling highest (`utils.py:83`, `_backend.py:2022`).
pub fn highest_k(data: &[f64], period: usize) -> Vec<f64> {
    roll(data, period, max_of)
}

/// Rolling lowest (`utils.py:129`, `_backend.py:2029`).
pub fn lowest_k(data: &[f64], period: usize) -> Vec<f64> {
    roll(data, period, min_of)
}

/// `_backend.py:299` `williams_r`.
pub fn williams_r_k(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let hh = highest_k(high, period);
    let ll = lowest_k(low, period);
    let mut out = vec![NAN; close.len()];
    for i in period.saturating_sub(1)..close.len() {
        out[i] = if hh[i] != ll[i] {
            -100.0 * (hh[i] - close[i]) / (hh[i] - ll[i])
        } else {
            -50.0
        };
    }
    out
}

/// `_backend.py:249` `stochastic`: slow %K = SMA(fast %K, smooth_k),
/// %D = SMA(slow %K, d_period).
pub fn stochastic_k(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    k_period: usize,
    smooth_k: usize,
    d_period: usize,
) -> (Vec<f64>, Vec<f64>) {
    let n = close.len();
    let hh = highest_k(high, k_period);
    let ll = lowest_k(low, k_period);
    let fk = k_period - 1;
    let mut fast = vec![NAN; n];
    for i in fk..n {
        fast[i] = if hh[i] != ll[i] {
            100.0 * (close[i] - ll[i]) / (hh[i] - ll[i])
        } else {
            50.0
        };
    }
    let mut slow_k = vec![NAN; n];
    if fk < n {
        for (j, v) in sma_k(&fast[fk..], smooth_k).into_iter().enumerate() {
            slow_k[fk + j] = v;
        }
    }
    let off = fk + smooth_k - 1;
    let mut slow_d = vec![NAN; n];
    if off < n {
        for (j, v) in sma_k(&slow_k[off..], d_period).into_iter().enumerate() {
            slow_d[off + j] = v;
        }
    }
    (slow_k, slow_d)
}

/// `_backend.py:2036` `supertrend`: (line, direction) with direction -1 up
/// and +1 down, as TradingView.
pub fn supertrend_k(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: usize,
    mult: f64,
) -> (Vec<f64>, Vec<f64>) {
    let n = close.len();
    let mut st = vec![NAN; n];
    let mut dr = vec![NAN; n];
    let atr = atr_k(high, low, close, period);
    let fv = period - 1;
    if fv >= n {
        return (st, dr);
    }
    let ub: Vec<f64> = (0..n)
        .map(|i| (high[i] + low[i]) / 2.0 + mult * atr[i])
        .collect();
    let lb: Vec<f64> = (0..n)
        .map(|i| (high[i] + low[i]) / 2.0 - mult * atr[i])
        .collect();
    let mut fu = vec![NAN; n];
    let mut fl = vec![NAN; n];
    fu[fv] = ub[fv];
    fl[fv] = lb[fv];
    dr[fv] = 1.0;
    st[fv] = ub[fv];
    for i in fv + 1..n {
        fl[i] = if lb[i] > fl[i - 1] || close[i - 1] < fl[i - 1] {
            lb[i]
        } else {
            fl[i - 1]
        };
        fu[i] = if ub[i] < fu[i - 1] || close[i - 1] > fu[i - 1] {
            ub[i]
        } else {
            fu[i - 1]
        };
        dr[i] = if st[i - 1] == fu[i - 1] {
            if close[i] > fu[i] {
                -1.0
            } else {
                1.0
            }
        } else if close[i] < fl[i] {
            1.0
        } else {
            -1.0
        };
        st[i] = if dr[i] == -1.0 { fl[i] } else { fu[i] };
    }
    (st, dr)
}

/// `_backend.py:2101` `ichimoku`: conversion, base, leading A and B
/// (shifted forward `disp - 1`), lagging (shifted back `disp - 1`).
pub fn ichimoku_k(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    conv: usize,
    base: usize,
    span2: usize,
    disp: i64,
) -> Vec<Vec<f64>> {
    let n = close.len();
    let don = |p: usize| -> Vec<f64> {
        let h = highest_k(high, p);
        let l = lowest_k(low, p);
        (0..n).map(|i| (h[i] + l[i]) / 2.0).collect()
    };
    let conversion = don(conv);
    let base_line = don(base);
    let lead1: Vec<f64> = (0..n)
        .map(|i| (conversion[i] + base_line[i]) / 2.0)
        .collect();
    let lead2 = don(span2);
    let off = disp - 1;
    let shift_fwd = |src: &[f64]| -> Vec<f64> {
        let mut out = vec![NAN; n];
        if off == 0 {
            out.copy_from_slice(src);
        } else if off > 0 && (off as usize) < n {
            let o = off as usize;
            out[o..].copy_from_slice(&src[..n - o]);
        }
        out
    };
    let la = shift_fwd(&lead1);
    let lb = shift_fwd(&lead2);
    let mut lag = vec![NAN; n];
    let offlag = -disp + 1;
    if offlag < 0 {
        let sh = offlag.unsigned_abs() as usize;
        if sh < n {
            lag[..n - sh].copy_from_slice(&close[sh..]);
        }
    } else if offlag > 0 {
        let o = offlag as usize;
        if o < n {
            lag[o..].copy_from_slice(&close[..n - o]);
        }
    } else {
        lag.copy_from_slice(close);
    }
    vec![conversion, base_line, la, lb, lag]
}

/// `_backend.py:728` `adx`: (+DI, -DI, ADX).
pub fn adx_k(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: usize,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = close.len();
    let tr = true_range_k(high, low, close);
    let mut dmp = vec![0.0; n];
    let mut dmm = vec![0.0; n];
    for i in 1..n {
        let up = high[i] - high[i - 1];
        let dn = low[i - 1] - low[i];
        dmp[i] = if up > dn && up > 0.0 { up } else { 0.0 };
        dmm[i] = if dn > up && dn > 0.0 { dn } else { 0.0 };
    }
    let sa = ema_wilder_k(&tr, period);
    let sp = ema_wilder_k(&dmp, period);
    let sm = ema_wilder_k(&dmm, period);
    let mut dip = vec![NAN; n];
    let mut dim = vec![NAN; n];
    let mut dx = vec![NAN; n];
    for i in period.saturating_sub(1)..n {
        if !sa[i].is_nan() && sa[i] > 0.0 {
            dip[i] = sp[i] / sa[i] * 100.0;
            dim[i] = sm[i] / sa[i] * 100.0;
            let s = dip[i] + dim[i];
            if s > 0.0 {
                dx[i] = (dip[i] - dim[i]).abs() / s * 100.0;
            }
        }
    }
    let adx = ema_wilder_k(&dx, period);
    (dip, dim, adx)
}

/// `_backend.py:781` `pivot_points`: pivot, r1, s1, r2, s2, r3, s3 per bar.
pub fn pivot_points_k(high: &[f64], low: &[f64], close: &[f64]) -> Vec<Vec<f64>> {
    let n = close.len();
    let mut out = vec![vec![NAN; n]; 7];
    for i in 0..n {
        let (h, l) = (high[i], low[i]);
        let p = (h + l + close[i]) / 3.0;
        out[0][i] = p;
        out[1][i] = 2.0 * p - l;
        out[2][i] = 2.0 * p - h;
        out[3][i] = p + (h - l);
        out[4][i] = p - (h - l);
        out[5][i] = h + 2.0 * (p - l);
        out[6][i] = l - 2.0 * (h - p);
    }
    out
}

/// `_backend.py:891` `_linreg_end`: the fitted value at the window's end.
fn linreg_end(y: &[f64]) -> f64 {
    let p = y.len() as f64;
    let sx: f64 = (0..y.len()).map(|x| x as f64).sum();
    let sy: f64 = y.iter().sum();
    let sxy: f64 = y.iter().enumerate().map(|(x, v)| x as f64 * v).sum();
    let sx2: f64 = (0..y.len()).map(|x| (x * x) as f64).sum();
    let den = p * sx2 - sx * sx;
    if den != 0.0 {
        let slope = (p * sxy - sx * sy) / den;
        let intercept = (sy - slope * sx) / p;
        slope * (p - 1.0) + intercept
    } else {
        y[y.len() - 1]
    }
}

/// `_backend.py:919` `lrslope`: change of the regression end value per bar.
pub fn lrslope_k(data: &[f64], period: usize, interval: f64) -> Vec<f64> {
    let n = data.len();
    let mut out = vec![NAN; n];
    for i in period..n {
        out[i] =
            (linreg_end(&data[i + 1 - period..=i]) - linreg_end(&data[i - period..i])) / interval;
    }
    out
}

/// `_backend.py:950` `correl`: rolling Pearson correlation.
pub fn correl_k(a: &[f64], b: &[f64], period: usize) -> Vec<f64> {
    let n = a.len();
    let mut out = vec![NAN; n];
    if period == 0 {
        return out;
    }
    for i in period - 1..n {
        let x = &a[i + 1 - period..=i];
        let y = &b[i + 1 - period..=i];
        let (mx, my) = (mean(x), mean(y));
        let num: f64 = x.iter().zip(y).map(|(p, q)| (p - mx) * (q - my)).sum();
        let sxx: f64 = x.iter().map(|p| (p - mx) * (p - mx)).sum();
        let syy: f64 = y.iter().map(|q| (q - my) * (q - my)).sum();
        let den = (sxx * syy).sqrt();
        out[i] = if den > 0.0 { num / den } else { 0.0 };
    }
    out
}

/// `_backend.py:967` `beta`: rolling covariance of price changes over the
/// market's variance.
pub fn beta_k(asset: &[f64], market: &[f64], period: usize) -> Vec<f64> {
    let n = asset.len();
    let mut out = vec![NAN; n];
    let mut ar = vec![NAN; n];
    let mut mr = vec![NAN; n];
    for i in 1..n {
        ar[i] = asset[i] - asset[i - 1];
        mr[i] = market[i] - market[i - 1];
    }
    for i in period..n {
        let aw = &ar[i + 1 - period..=i];
        let mw = &mr[i + 1 - period..=i];
        let (ma, mm) = (mean(aw), mean(mw));
        let mut cov = 0.0;
        let mut var = 0.0;
        for j in 0..period {
            cov += (aw[j] - ma) * (mw[j] - mm);
            var += (mw[j] - mm) * (mw[j] - mm);
        }
        cov /= period as f64;
        var /= period as f64;
        out[i] = if var > 0.0 { cov / var } else { 0.0 };
    }
    out
}

/// `_backend.py:1884` `hv`: annualised stdev of log returns, in percent.
pub fn hv_k(close: &[f64], length: usize, annual: f64, per: f64) -> Vec<f64> {
    let n = close.len();
    let mut lr = vec![NAN; n];
    for i in 1..n {
        if close[i - 1] > 0.0 && close[i] > 0.0 {
            lr[i] = (close[i] / close[i - 1]).ln();
        }
    }
    stdev_k(&lr, length)
        .into_iter()
        .map(|s| 100.0 * s * (annual / per).sqrt())
        .collect()
}

/// `utils.py:25` `crossover`: `a` crosses above `b` on this bar.
pub fn crossover(a: &[f64], b: &[f64]) -> Vec<bool> {
    let mut out = vec![false; a.len()];
    for i in 1..a.len() {
        let ok = ![a[i], b[i], a[i - 1], b[i - 1]].iter().any(|x| x.is_nan());
        out[i] = ok && a[i] > b[i] && a[i - 1] <= b[i - 1];
    }
    out
}

/// `utils.py:54` `crossunder`.
pub fn crossunder(a: &[f64], b: &[f64]) -> Vec<bool> {
    let mut out = vec![false; a.len()];
    for i in 1..a.len() {
        let ok = ![a[i], b[i], a[i - 1], b[i - 1]].iter().any(|x| x.is_nan());
        out[i] = ok && a[i] < b[i] && a[i - 1] >= b[i - 1];
    }
    out
}

// ----------------------------------------------------------------------
// The `ta.<name>(...)` surface: argument binding and wrapper checks
// ----------------------------------------------------------------------

/// An indicator's result: one series or a tuple of series.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    One(Vec<f64>),
    Many(Vec<Vec<f64>>),
}

/// A scalar parameter of a `ta` function.
#[derive(Debug, Clone, Copy)]
struct Sc {
    name: &'static str,
    /// Default, or `None` when required.
    default: Option<f64>,
    int: bool,
}

const fn int(name: &'static str, d: Option<f64>) -> Sc {
    Sc {
        name,
        default: d,
        int: true,
    }
}
const fn flt(name: &'static str, d: f64) -> Sc {
    Sc {
        name,
        default: Some(d),
        int: false,
    }
}

/// Signature of one supported function: series parameters, then scalars.
struct Sig {
    name: &'static str,
    series: &'static [&'static str],
    scalars: &'static [Sc],
}

const HLC: &[&str] = &["high", "low", "close"];

/// Every `ta` function the desktop computes (names as in `openalgo.ta`).
const SIGS: &[Sig] = &[
    Sig {
        name: "sma",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "ema",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "wma",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "rsi",
        series: &["data"],
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "macd",
        series: &["data"],
        scalars: &[
            int("fast_period", Some(12.0)),
            int("slow_period", Some(26.0)),
            int("signal_period", Some(9.0)),
        ],
    },
    Sig {
        name: "supertrend",
        series: HLC,
        scalars: &[int("period", Some(10.0)), flt("multiplier", 3.0)],
    },
    Sig {
        name: "ichimoku",
        series: HLC,
        scalars: &[
            int("conversion_periods", Some(9.0)),
            int("base_periods", Some(26.0)),
            int("lagging_span2_periods", Some(52.0)),
            int("displacement", Some(26.0)),
        ],
    },
    Sig {
        name: "stochastic",
        series: HLC,
        scalars: &[
            int("k_period", Some(14.0)),
            int("smooth_k", Some(3.0)),
            int("d_period", Some(3.0)),
        ],
    },
    Sig {
        name: "cci",
        series: HLC,
        scalars: &[int("period", Some(20.0))],
    },
    Sig {
        name: "williams_r",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "atr",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "natr",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "true_range",
        series: HLC,
        scalars: &[],
    },
    Sig {
        name: "bbands",
        series: &["data"],
        scalars: &[int("period", Some(20.0)), flt("std_dev", 2.0)],
    },
    Sig {
        name: "bbpercent",
        series: &["data"],
        scalars: &[int("period", Some(20.0)), flt("std_dev", 2.0)],
    },
    Sig {
        name: "bbwidth",
        series: &["data"],
        scalars: &[int("period", Some(20.0)), flt("std_dev", 2.0)],
    },
    Sig {
        name: "keltner",
        series: HLC,
        scalars: &[
            int("ema_period", Some(20.0)),
            int("atr_period", Some(10.0)),
            flt("multiplier", 2.0),
        ],
    },
    Sig {
        name: "donchian",
        series: &["high", "low"],
        scalars: &[int("period", Some(20.0))],
    },
    Sig {
        name: "hv",
        series: &["close"],
        scalars: &[
            int("length", Some(10.0)),
            int("annual", Some(365.0)),
            int("per", Some(1.0)),
        ],
    },
    Sig {
        name: "adx",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "pivot_points",
        series: HLC,
        scalars: &[],
    },
    Sig {
        name: "highest",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "lowest",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "stdev",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "lrslope",
        series: &["data"],
        scalars: &[int("period", Some(100.0)), int("interval", Some(1.0))],
    },
    Sig {
        name: "correlation",
        series: &["data1", "data2"],
        scalars: &[int("period", Some(20.0))],
    },
    Sig {
        name: "beta",
        series: &["asset", "market"],
        scalars: &[int("period", Some(252.0))],
    },
];

/// Every public function of the web's `openalgo.ta` (2.0.5), so a name the
/// desktop does not compute yet is told apart from one that does not exist.
pub const WEB_TA_FUNCTIONS: &[&str] = &[
    "accelerator_oscillator",
    "adl",
    "adx",
    "adxr",
    "alligator",
    "alma",
    "apo",
    "aroon",
    "aroon_oscillator",
    "atr",
    "avgprice",
    "awesome_oscillator",
    "bbands",
    "bbpercent",
    "bbwidth",
    "beta",
    "bop",
    "cci",
    "chaikin",
    "chandelier_exit",
    "change",
    "cho",
    "chop",
    "ckstop",
    "cmf",
    "cmo",
    "coppock",
    "correlation",
    "cross",
    "crossover",
    "crossunder",
    "crsi",
    "dema",
    "dmi",
    "donchian",
    "dpo",
    "dx",
    "elderray",
    "ema",
    "emv",
    "exrem",
    "falling",
    "fisher",
    "flip",
    "force_index",
    "fractals",
    "frama",
    "gator_oscillator",
    "highest",
    "hma",
    "hv",
    "ichimoku",
    "kama",
    "keltner",
    "kst",
    "kvo",
    "linreg",
    "linregangle",
    "linregintercept",
    "lowest",
    "lrslope",
    "ma_envelopes",
    "macd",
    "massindex",
    "mcginley",
    "median",
    "median_bands",
    "medprice",
    "mfi",
    "midpoint",
    "midprice",
    "minus_dm",
    "mode",
    "mom",
    "natr",
    "nvi",
    "nvi_with_ema",
    "obv",
    "obv_smoothed",
    "pivot_points",
    "plus_dm",
    "po",
    "ppo",
    "psar",
    "pvi",
    "pvi_with_signal",
    "pvt",
    "rising",
    "roc",
    "rocp",
    "rocr",
    "rocr100",
    "rsi",
    "rvi",
    "rvol",
    "rwi",
    "sma",
    "starc",
    "stc",
    "stdev",
    "stochastic",
    "stochf",
    "stochrsi",
    "supertrend",
    "t3",
    "tema",
    "trima",
    "trix",
    "true_range",
    "tsf",
    "tsi",
    "typprice",
    "ulcerindex",
    "ultimate_oscillator",
    "uo_oscillator",
    "valuewhen",
    "variance",
    "vi",
    "vidya",
    "volosc",
    "vroc",
    "vwap",
    "vwma",
    "wclprice",
    "williams_r",
    "wma",
    "zlema",
];

/// Names the desktop computes.
pub fn supported() -> Vec<&'static str> {
    SIGS.iter().map(|s| s.name).collect()
}

pub fn is_supported(name: &str) -> bool {
    SIGS.iter().any(|s| s.name == name)
}

/// One bound argument.
enum Arg<'a> {
    Series(&'a [f64]),
    Num(f64),
}

fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 required positional argument: {}", word)
    } else {
        format!("{} required positional arguments: {}", n, word)
    }
}

fn names_list(names: &[&str]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("'{}'", n)).collect();
    match q.len() {
        0 => String::new(),
        1 => q[0].clone(),
        _ => format!("{} and {}", q[..q.len() - 1].join(", "), q[q.len() - 1]),
    }
}

/// `ta.<name>(*series, **params)` as the web's tools call it.
pub fn call(name: &str, series: &[&[f64]], params: &Map<String, Value>) -> R<Out> {
    let sig = SIGS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| PyErr::value(format!("unknown indicator '{}'", name)))?;
    let all: Vec<&str> = sig
        .series
        .iter()
        .copied()
        .chain(sig.scalars.iter().map(|s| s.name))
        .collect();
    if series.len() > all.len() {
        return Err(PyErr::type_err(format!(
            "{}() takes from {} to {} positional arguments but {} were given",
            name,
            sig.series.len() + 1,
            all.len() + 1,
            series.len() + 1
        )));
    }
    let mut bound: Vec<Option<Arg>> = (0..all.len()).map(|_| None).collect();
    for (i, s) in series.iter().enumerate() {
        bound[i] = Some(Arg::Series(s));
    }
    for (k, v) in params {
        let Some(pos) = all.iter().position(|n| n == k) else {
            return Err(PyErr::type_err(format!(
                "{}() got an unexpected keyword argument '{}'",
                name, k
            )));
        };
        if bound[pos].is_some() {
            return Err(PyErr::type_err(format!(
                "{}() got multiple values for argument '{}'",
                name, k
            )));
        }
        if pos < sig.series.len() {
            return Err(PyErr::type_err(format!(
                "Invalid input type: <class '{}'>. Expected np.ndarray, pd.Series, or list",
                py_type(v)
            )));
        }
        let sc = sig.scalars[pos - sig.series.len()];
        let num = match v {
            Value::Number(n) if sc.int && !(n.is_i64() || n.is_u64()) => {
                return Err(PyErr::type_err(format!(
                    "Period must be an integer, got <class '{}'>",
                    py_type(v)
                )))
            }
            Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
            Value::Bool(b) if !sc.int => f64::from(u8::from(*b)),
            _ => {
                return Err(PyErr::type_err(format!(
                    "'{}' must be a number, got <class '{}'>",
                    k,
                    py_type(v)
                )))
            }
        };
        bound[pos] = Some(Arg::Num(num));
    }
    let missing: Vec<&str> = all
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            bound[*i].is_none()
                && (*i < sig.series.len() || sig.scalars[*i - sig.series.len()].default.is_none())
        })
        .map(|(_, n)| *n)
        .collect();
    if !missing.is_empty() {
        return Err(PyErr::type_err(format!(
            "{}() missing {}",
            name,
            plural(missing.len(), &names_list(&missing))
        )));
    }
    let mut ser: Vec<&[f64]> = Vec::new();
    for b in bound.iter().take(sig.series.len()) {
        match b {
            Some(Arg::Series(s)) => ser.push(s),
            _ => return Err(PyErr::type_err("Invalid input type")),
        }
    }
    let mut sc: Vec<f64> = Vec::new();
    for (j, s) in sig.scalars.iter().enumerate() {
        match &bound[sig.series.len() + j] {
            Some(Arg::Num(x)) => sc.push(*x),
            Some(Arg::Series(_)) => {
                return Err(PyErr::type_err(
                    "Period must be an integer, got <class 'pandas.core.series.Series'>",
                ))
            }
            None => sc.push(s.default.unwrap_or(f64::NAN)),
        }
    }
    for s in &ser {
        non_empty(s)?;
    }
    let len = ser[0].len();
    if ser.iter().any(|s| s.len() != len) {
        return Err(PyErr::value("All input arrays must have the same length"));
    }
    compute(name, &ser, &sc, len)
}

fn period(x: f64) -> i64 {
    x as i64
}

fn compute(name: &str, s: &[&[f64]], sc: &[f64], len: usize) -> R<Out> {
    let p0 = period(sc.first().copied().unwrap_or(0.0));
    let up = |p: i64| p as usize;
    Ok(match name {
        "sma" => {
            validate_period(p0, len)?;
            Out::One(sma_k(s[0], up(p0)))
        }
        "ema" => {
            validate_period(p0, len)?;
            Out::One(ema_k(s[0], up(p0)))
        }
        "wma" => {
            validate_period(p0, len)?;
            Out::One(wma_k(s[0], up(p0)))
        }
        "stdev" => {
            validate_period(p0, len)?;
            Out::One(stdev_k(s[0], up(p0)))
        }
        "rsi" => {
            validate_period(p0, len)?;
            Out::One(rsi_k(s[0], up(p0)))
        }
        "macd" => {
            let (f, sl, sg) = (period(sc[0]), period(sc[1]), period(sc[2]));
            if f <= 0 || sl <= 0 || sg <= 0 {
                return Err(PyErr::value("All periods must be positive"));
            }
            if f >= sl {
                return Err(PyErr::value("Fast period must be less than slow period"));
            }
            let line: Vec<f64> = ema_k(s[0], up(f))
                .iter()
                .zip(ema_k(s[0], up(sl)))
                .map(|(a, b)| a - b)
                .collect();
            let signal = ema_k(&line, up(sg));
            let hist = line.iter().zip(&signal).map(|(a, b)| a - b).collect();
            Out::Many(vec![line, signal, hist])
        }
        "supertrend" => {
            validate_period(p0, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Multiplier must be positive, got {}",
                    sc[1]
                )));
            }
            let (a, b) = supertrend_k(s[0], s[1], s[2], up(p0), sc[1]);
            Out::Many(vec![a, b])
        }
        "ichimoku" => {
            for (v, n) in [
                (sc[0], "conversion_periods"),
                (sc[1], "base_periods"),
                (sc[2], "lagging_span2_periods"),
            ] {
                if v <= 0.0 {
                    return Err(PyErr::value(format!(
                        "{} must be positive, got {}",
                        n,
                        period(v)
                    )));
                }
            }
            Out::Many(ichimoku_k(
                s[0],
                s[1],
                s[2],
                up(period(sc[0])),
                up(period(sc[1])),
                up(period(sc[2])),
                period(sc[3]),
            ))
        }
        "stochastic" => {
            validate_period(p0, len)?;
            if sc[2] <= 0.0 {
                return Err(PyErr::value(format!(
                    "d_period must be positive, got {}",
                    period(sc[2])
                )));
            }
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "smooth_k must be positive, got {}",
                    period(sc[1])
                )));
            }
            let (k, d) = stochastic_k(
                s[0],
                s[1],
                s[2],
                up(p0),
                up(period(sc[1])),
                up(period(sc[2])),
            );
            Out::Many(vec![k, d])
        }
        "cci" => {
            validate_period(p0, len)?;
            Out::One(cci_k(s[0], s[1], s[2], up(p0)))
        }
        "williams_r" => {
            validate_period(p0, len)?;
            Out::One(williams_r_k(s[0], s[1], s[2], up(p0)))
        }
        "atr" => {
            validate_period(p0, len)?;
            Out::One(atr_k(s[0], s[1], s[2], up(p0)))
        }
        "natr" => {
            validate_period(p0, len)?;
            let atr = atr_k(s[0], s[1], s[2], up(p0));
            Out::One(
                atr.iter()
                    .zip(s[2].iter())
                    .map(|(a, c)| if *c != 0.0 { a / c * 100.0 } else { 0.0 })
                    .collect(),
            )
        }
        "true_range" => Out::One(true_range_k(s[0], s[1], s[2])),
        "bbands" => {
            validate_period(p0, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Standard deviation multiplier must be positive, got {}",
                    sc[1]
                )));
            }
            let (u, m, l) = bands(s[0], up(p0), sc[1]);
            Out::Many(vec![u, m, l])
        }
        "bbpercent" => {
            validate_period(p0, len)?;
            let (u, _m, l) = bands(s[0], up(p0), sc[1]);
            Out::One(
                (0..len)
                    .map(|i| {
                        if !u[i].is_nan() && u[i] == l[i] {
                            0.5
                        } else {
                            (s[0][i] - l[i]) / (u[i] - l[i])
                        }
                    })
                    .collect(),
            )
        }
        "bbwidth" => {
            validate_period(p0, len)?;
            let (u, m, l) = bands(s[0], up(p0), sc[1]);
            Out::One(
                (0..len)
                    .map(|i| {
                        if !m[i].is_nan() && m[i] == 0.0 {
                            0.0
                        } else {
                            (u[i] - l[i]) / m[i]
                        }
                    })
                    .collect(),
            )
        }
        "keltner" => {
            let (ep, ap) = (period(sc[0]), period(sc[1]));
            validate_period(ep, len)?;
            validate_period(ap, len)?;
            if sc[2] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Multiplier must be positive, got {}",
                    sc[2]
                )));
            }
            let mid = ema_sma_k(s[2], up(ep));
            let atr = atr_k(s[0], s[1], s[2], up(ap));
            let upper = (0..len).map(|i| mid[i] + sc[2] * atr[i]).collect();
            let lower = (0..len).map(|i| mid[i] - sc[2] * atr[i]).collect();
            Out::Many(vec![upper, mid, lower])
        }
        "donchian" => {
            validate_period(p0, len)?;
            let u = highest_k(s[0], up(p0));
            let l = lowest_k(s[1], up(p0));
            let m = (0..len).map(|i| (u[i] + l[i]) / 2.0).collect();
            Out::Many(vec![u, m, l])
        }
        "hv" => {
            validate_period(p0 + 1, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Annual periods must be positive, got {}",
                    period(sc[1])
                )));
            }
            if sc[2] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Per periods must be positive, got {}",
                    period(sc[2])
                )));
            }
            Out::One(hv_k(s[0], up(p0), sc[1], sc[2]))
        }
        "adx" => {
            validate_period(p0, len)?;
            let (a, b, c) = adx_k(s[0], s[1], s[2], up(p0));
            Out::Many(vec![a, b, c])
        }
        "pivot_points" => Out::Many(pivot_points_k(s[0], s[1], s[2])),
        "highest" => Out::One(highest_k(s[0], up(p0.max(0)))),
        "lowest" => Out::One(lowest_k(s[0], up(p0.max(0)))),
        "lrslope" => {
            validate_period(p0 + 1, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Interval must be positive, got {}",
                    period(sc[1])
                )));
            }
            Out::One(lrslope_k(s[0], up(p0), sc[1]))
        }
        "correlation" => {
            validate_period(p0, len)?;
            Out::One(correl_k(s[0], s[1], up(p0)))
        }
        "beta" => {
            validate_period(p0 + 1, len)?;
            Out::One(beta_k(s[0], s[1], up(p0)))
        }
        other => return Err(PyErr::value(format!("unknown indicator '{}'", other))),
    })
}

/// `_backend.py:241` `bbands`: SMA middle, population stdev bands.
fn bands(data: &[f64], period: usize, k: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let m = sma_k(data, period);
    let sd = stdev_k(data, period);
    let u = m.iter().zip(&sd).map(|(a, b)| a + k * b).collect();
    let l = m.iter().zip(&sd).map(|(a, b)| a - k * b).collect();
    (u, m, l)
}

/// Python `round(x, nd)` (correctly rounded on the binary value).
pub fn py_round(x: f64, nd: usize) -> f64 {
    format!("{:.*}", nd, x).parse().unwrap_or(x)
}

/// Web `_last`: the last non-NaN value rounded to 4 places.
pub fn last(series: &[f64]) -> Option<f64> {
    series
        .iter()
        .rev()
        .find(|x| !x.is_nan())
        .map(|x| py_round(*x, 4))
}

#[cfg(test)]
mod tests {
    //! Reference values derived by hand from the formulas in
    //! `openalgo/indicators/_backend.py` (line numbers on each test).
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 || (a.is_nan() && b.is_nan())
    }

    fn same(got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{:?}", got);
        for (g, w) in got.iter().zip(want) {
            assert!(close(*g, *w), "got {:?} want {:?}", got, want);
        }
    }

    #[test]
    fn sma_window_mean() {
        // _backend.py:75: mean of each full window; NaN before period-1.
        same(
            &sma_k(&[1.0, 2.0, 3.0, 4.0, 5.0], 3),
            &[NAN, NAN, 2.0, 3.0, 4.0],
        );
        // Window-local NaN: only the windows holding the NaN are blank.
        same(
            &sma_k(&[1.0, NAN, 3.0, 4.0, 5.0], 2),
            &[NAN, NAN, NAN, 3.5, 4.5],
        );
    }

    #[test]
    fn ema_seeds_at_first_value() {
        // _backend.py:128: alpha = 2/(2+1); e0 = 1, e1 = 2/3*2 + 1/3*1 = 5/3,
        // e2 = 2/3*3 + 1/3*5/3 = 23/9.
        same(&ema_k(&[1.0, 2.0, 3.0], 2), &[1.0, 5.0 / 3.0, 23.0 / 9.0]);
    }

    #[test]
    fn wma_linear_weights() {
        // _backend.py:89: weights 1,2,3 over sum 6: (1+4+9)/6, (2+6+12)/6.
        same(
            &wma_k(&[1.0, 2.0, 3.0, 4.0], 3),
            &[NAN, NAN, 14.0 / 6.0, 20.0 / 6.0],
        );
    }

    #[test]
    fn rsi_wilder() {
        // _backend.py:201, period 2 on [1,2,3,2]: deltas 1,1,-1. Seed
        // gains mean(1,1)=1, losses 0 -> 100 at index 2. Then gain
        // (1*1+0)/2=0.5, loss (0+1)/2=0.5 -> RS 1 -> 50 at index 3.
        same(&rsi_k(&[1.0, 2.0, 3.0, 2.0], 2), &[NAN, NAN, 100.0, 50.0]);
    }

    #[test]
    fn true_range_and_atr() {
        // _backend.py:164: tr0 = h-l; tri = max(h-l, |h-c1|, |l-c1|).
        let (h, l, c) = ([10.0, 11.0, 13.0], [8.0, 9.0, 10.0], [9.0, 10.0, 12.0]);
        // tr = [2, max(2,2,0)=2, max(3,3,0)=3]
        same(&true_range_k(&h, &l, &c), &[2.0, 2.0, 3.0]);
        // _backend.py:186/406: seed mean(2,2)=2 at 1, then (2*1+3)/2=2.5.
        same(&atr_k(&h, &l, &c, 2), &[NAN, 2.0, 2.5]);
    }

    #[test]
    fn bollinger_population_stdev() {
        // _backend.py:241/148: mean 2, var (1+4+9)/3 - 4 = 2/3.
        let sd = (2.0f64 / 3.0).sqrt();
        let out = call(
            "bbands",
            &[&[1.0, 2.0, 3.0]],
            json!({"period": 3}).as_object().unwrap(),
        )
        .unwrap();
        match out {
            Out::Many(v) => {
                same(&v[0], &[NAN, NAN, 2.0 + 2.0 * sd]);
                same(&v[1], &[NAN, NAN, 2.0]);
                same(&v[2], &[NAN, NAN, 2.0 - 2.0 * sd]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn macd_is_ema_difference() {
        // _backend.py:234: line = ema(fast) - ema(slow); signal = ema(line).
        let d = [1.0, 2.0, 3.0];
        let out = call(
            "macd",
            &[&d],
            json!({"fast_period": 1, "slow_period": 2, "signal_period": 2})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        // ema(1) = data; ema(2) = [1, 5/3, 23/9]; line = [0, 1/3, 4/9].
        // signal: [0, 2/3*1/3 + 0 = 2/9, 2/3*4/9 + 1/3*2/9 = 10/27].
        match out {
            Out::Many(v) => {
                same(&v[0], &[0.0, 1.0 / 3.0, 4.0 / 9.0]);
                same(&v[1], &[0.0, 2.0 / 9.0, 10.0 / 27.0]);
                same(&v[2], &[0.0, 1.0 / 9.0, 4.0 / 9.0 - 10.0 / 27.0]);
            }
            _ => panic!(),
        }
        let e = call(
            "macd",
            &[&d],
            json!({"fast_period": 3, "slow_period": 2})
                .as_object()
                .unwrap(),
        )
        .unwrap_err();
        assert_eq!(e.message, "Fast period must be less than slow period");
    }

    #[test]
    fn supertrend_flips_up_on_close_above_band() {
        // _backend.py:2036 with period 1, multiplier 1: atr = tr.
        // bar0: tr 2, hl2 9 -> ub 11, lb 7, dir +1, st 11.
        // bar1: h 14 l 12 c 13.5: tr max(2, |14-9|=5, |12-9|=3)=5 -> hl2 13,
        //   ub 18, lb 8. fl: 8 > 7 -> 8. fu: 18 < 11? no; c0 9 > 11? no -> 11.
        //   st0 == fu0 so dir = close 13.5 > 11 -> -1 (up); st = fl = 8.
        let (st, dr) = supertrend_k(&[10.0, 14.0], &[8.0, 12.0], &[9.0, 13.5], 1, 1.0);
        same(&st, &[11.0, 8.0]);
        same(&dr, &[1.0, -1.0]);
    }

    #[test]
    fn stochastic_and_williams() {
        // _backend.py:249 k=2, smooth 1, d 1: fast k at 1 = 100*(c-ll)/(hh-ll).
        let (h, l, c) = ([10.0, 12.0], [8.0, 9.0], [9.0, 11.0]);
        let (k, d) = stochastic_k(&h, &l, &c, 2, 1, 1);
        // hh 12, ll 8 -> 100*(11-8)/4 = 75.
        same(&k, &[NAN, 75.0]);
        same(&d, &[NAN, 75.0]);
        // _backend.py:299: -100*(12-11)/4 = -25.
        same(&williams_r_k(&h, &l, &c, 2), &[NAN, -25.0]);
    }

    #[test]
    fn cci_mean_deviation() {
        // _backend.py:267, period 2: tp = [3, 6]; mean 4.5; md 1.5;
        // (6 - 4.5) / (0.015 * 1.5) = 66.666...
        same(
            &cci_k(&[4.0, 7.0], &[2.0, 5.0], &[3.0, 6.0], 2),
            &[NAN, 1.5 / 0.0225],
        );
    }

    #[test]
    fn pivot_points_classic() {
        // _backend.py:781: h 12 l 8 c 10 -> p 10, r1 12, s1 8, r2 14, s2 6, r3 16, s3 4.
        let p = pivot_points_k(&[12.0], &[8.0], &[10.0]);
        let flat: Vec<f64> = p.iter().map(|s| s[0]).collect();
        same(&flat, &[10.0, 12.0, 8.0, 14.0, 6.0, 16.0, 4.0]);
    }

    #[test]
    fn regression_correlation_beta() {
        // _backend.py:919: on a straight line the fitted end value moves by
        // the slope each bar -> 2.
        same(
            &lrslope_k(&[1.0, 3.0, 5.0, 7.0], 2, 1.0),
            &[NAN, NAN, 2.0, 2.0],
        );
        // _backend.py:950: y = 2x is perfectly correlated.
        same(
            &correl_k(&[1.0, 2.0, 4.0], &[2.0, 4.0, 8.0], 3),
            &[NAN, NAN, 1.0],
        );
        // _backend.py:967: asset changes are twice the market's -> beta 2.
        same(
            &beta_k(&[1.0, 3.0, 4.0], &[1.0, 2.0, 2.5], 2),
            &[NAN, NAN, 2.0],
        );
    }

    #[test]
    fn adx_on_steady_uptrend() {
        // _backend.py:728: every bar moves up by 1 with range 2, so -DM is 0,
        // +DI = 100 * mean(+DM) / ATR and DX = 100 once both are seeded.
        let h = [10.0, 11.0, 12.0, 13.0];
        let l = [8.0, 9.0, 10.0, 11.0];
        let c = [9.0, 10.0, 11.0, 12.0];
        let (p, m, a) = adx_k(&h, &l, &c, 2);
        // tr = [2, 2, 2, 2]; +dm = [0, 1, 1, 1]; rma(2): tr [_, 2, 2, 2],
        // +dm [_, 0.5, 0.75, 0.875] -> +DI [_, 25, 37.5, 43.75]; -DI 0;
        // dx 100 from index 1 -> adx seeded mean(100,100) at index 2.
        same(&p, &[NAN, 25.0, 37.5, 43.75]);
        same(&m, &[NAN, 0.0, 0.0, 0.0]);
        same(&a, &[NAN, NAN, 100.0, 100.0]);
    }

    #[test]
    fn hv_annualised_log_returns() {
        // _backend.py:1884, length 2: returns ln2, ln2 -> stdev 0.
        same(&hv_k(&[1.0, 2.0, 4.0], 2, 365.0, 1.0), &[NAN, NAN, 0.0]);
    }

    #[test]
    fn ichimoku_shifts() {
        // _backend.py:2101, conv 1, base 1, span2 1, disp 2: lines are hl2,
        // leading spans shift forward 1, lagging span shifts back 1.
        let v = ichimoku_k(
            &[2.0, 4.0, 6.0],
            &[0.0, 2.0, 4.0],
            &[1.0, 3.0, 5.0],
            1,
            1,
            1,
            2,
        );
        same(&v[0], &[1.0, 3.0, 5.0]);
        same(&v[2], &[NAN, 1.0, 3.0]);
        same(&v[4], &[3.0, 5.0, NAN]);
    }

    #[test]
    fn crossings() {
        // utils.py:25/54.
        assert_eq!(
            crossover(&[1.0, 3.0, 2.0], &[2.0, 2.0, 2.0]),
            vec![false, true, false]
        );
        assert_eq!(
            crossunder(&[3.0, 1.0, 1.0], &[2.0, 2.0, 2.0]),
            vec![false, true, false]
        );
    }

    #[test]
    fn wrapper_errors_match_python() {
        let m = Map::new();
        let e = call("ema", &[&[1.0, 2.0]], &m).unwrap_err();
        assert_eq!(e.kind, "TypeError");
        assert_eq!(
            e.message,
            "ema() missing 1 required positional argument: 'period'"
        );
        let e = call("rsi", &[&[1.0, 2.0]], &m).unwrap_err();
        assert_eq!(
            e.message,
            "Period (14) cannot be greater than data length (2)"
        );
        let e = call(
            "rsi",
            &[&[1.0, 2.0]],
            json!({"length": 3}).as_object().unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            e.message,
            "rsi() got an unexpected keyword argument 'length'"
        );
        let e = call("atr", &[&[1.0]], &m).unwrap_err();
        assert_eq!(
            e.message,
            "atr() missing 2 required positional arguments: 'low' and 'close'"
        );
    }

    #[test]
    fn python_rounding() {
        assert_eq!(py_round(1.23456, 4), 1.2346);
        assert_eq!(last(&[1.0, 2.123456, NAN]), Some(2.1235));
        assert_eq!(last(&[NAN]), None);
    }

    /// Last values from the web's own `openalgo.ta` (2.0.5, compiled core)
    /// on the same 80-bar series, produced once with the web's Python
    /// environment. Every supported function must agree to 1e-9.
    #[test]
    fn agrees_with_the_web_library_on_a_long_series() {
        let n = 80;
        let close: Vec<f64> = (0..n)
            .map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.1)
            .collect();
        let high: Vec<f64> = (0..n)
            .map(|i| close[i] + 1.0 + (i as f64).cos().abs())
            .collect();
        let low: Vec<f64> = (0..n)
            .map(|i| close[i] - 1.0 - (i as f64 * 1.3).sin().abs())
            .collect();
        let mkt: Vec<f64> = (0..n)
            .map(|i| 50.0 + (i as f64 * 0.5).cos() * 3.0 + i as f64 * 0.05)
            .collect();
        let reference: Value = serde_json::from_str(
            r#"{"sma": 106.47148224510313, "ema": 106.42827576863061, "wma": 106.76969234365465, "rsi": 44.31344217891444, "macd": [0.16165225409203288, 0.6791402514451601, -0.5174879973531272], "supertrend": [99.93802401689173, -1.0], "adx": [22.400901919440468, 28.91445081977545, 11.29172308766731], "ichimoku": [107.4496400262411, 106.59300648520994, 104.66826703626509, 102.95856145274135, 103.15718153140847], "stochastic": [27.00251958205408, 43.10122857003811], "cci": -65.26987676503794, "williams_r": -78.4540803241933, "atr": 4.066581739072121, "natr": 3.942121797728606, "bbands": [113.84819896559179, 106.47148224510313, 99.09476552461447], "bbpercent": 0.27535393866434155, "bbwidth": 0.13856699587425542, "keltner": [114.55385370225757, 106.4287269781737, 98.30360025408983], "donchian": [113.56842732723733, 106.59300648520994, 99.61758564318257], "hv": 40.91355563580153, "pivot_points": [103.18039557828426, 105.02993843132367, 101.30763867836909, 106.90269533123885, 99.45809582532968, 108.75223818427826, 97.5853389254145], "lrslope": -0.9018372198264473, "correlation": 0.4578237642000194, "beta": 1.0352753615105659, "highest": 113.56842732723733, "lowest": 99.61758564318257}"#,
        )
        .unwrap();
        let p = |v: Value| v.as_object().cloned().unwrap();
        let hlc: [&[f64]; 3] = [&high, &low, &close];
        let cases: Vec<(&str, Vec<&[f64]>, Value)> = vec![
            ("sma", vec![&close], json!({"period": 20})),
            ("ema", vec![&close], json!({"period": 20})),
            ("wma", vec![&close], json!({"period": 10})),
            ("rsi", vec![&close], json!({"period": 14})),
            ("macd", vec![&close], json!({})),
            ("supertrend", hlc.to_vec(), json!({})),
            ("adx", hlc.to_vec(), json!({"period": 14})),
            ("ichimoku", hlc.to_vec(), json!({})),
            ("stochastic", hlc.to_vec(), json!({})),
            ("cci", hlc.to_vec(), json!({"period": 20})),
            ("williams_r", hlc.to_vec(), json!({"period": 14})),
            ("atr", hlc.to_vec(), json!({"period": 14})),
            ("natr", hlc.to_vec(), json!({"period": 14})),
            (
                "bbands",
                vec![&close],
                json!({"period": 20, "std_dev": 2.0}),
            ),
            (
                "bbpercent",
                vec![&close],
                json!({"period": 20, "std_dev": 2.0}),
            ),
            (
                "bbwidth",
                vec![&close],
                json!({"period": 20, "std_dev": 2.0}),
            ),
            ("keltner", hlc.to_vec(), json!({})),
            ("donchian", vec![&high, &low], json!({"period": 20})),
            ("hv", vec![&close], json!({})),
            ("pivot_points", hlc.to_vec(), json!({})),
            ("lrslope", vec![&close], json!({"period": 20})),
            ("correlation", vec![&close, &mkt], json!({"period": 20})),
            ("beta", vec![&close, &mkt], json!({"period": 20})),
            ("highest", vec![&high], json!({"period": 20})),
            ("lowest", vec![&low], json!({"period": 20})),
        ];
        assert_eq!(
            cases.len(),
            supported().len() - 2,
            "stdev and true_range are covered above"
        );
        let lastv = |s: &[f64]| s.iter().rev().find(|x| !x.is_nan()).copied().unwrap();
        for (name, series, params) in cases {
            let got: Vec<f64> = match call(name, &series, &p(params)).unwrap() {
                Out::One(v) => vec![lastv(&v)],
                Out::Many(v) => v.iter().map(|s| lastv(s)).collect(),
            };
            let want: Vec<f64> = match &reference[name] {
                Value::Array(a) => a.iter().map(|x| x.as_f64().unwrap()).collect(),
                x => vec![x.as_f64().unwrap()],
            };
            assert_eq!(got.len(), want.len(), "{}", name);
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    (g - w).abs() < 1e-9,
                    "{}: got {:?} want {:?}",
                    name,
                    got,
                    want
                );
            }
        }
    }

    #[test]
    fn web_list_covers_supported() {
        for n in supported() {
            assert!(WEB_TA_FUNCTIONS.contains(&n), "{}", n);
        }
        assert_eq!(WEB_TA_FUNCTIONS.len(), 127);
    }
}
