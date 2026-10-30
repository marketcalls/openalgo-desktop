//! `openalgo/indicators/statistics.py` (the 9 statistical methods) and the
//! `_backend.py` compositions they call.

use super::numpy::{map, nan_vec, sorted, zip};
use super::{many, non_empty, one, us, validate_period, Args, TaError, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "linreg" => single(a, oa_core::linreg),
        "tsf" => single(a, oa_core::tsf),
        "median" => single(a, oa_core::median),
        "lrslope" => lrslope(a),
        "correlation" => correlation(a),
        "beta" => beta(a),
        "variance" => variance(a),
        "median_bands" => median_bands(a),
        "mode" => mode(a),
        _ => return None,
    })
}

/// LINREG, TSF, MEDIAN: one series, `validate_period`, one kernel.
fn single(a: &Args, kernel: fn(&[f64], usize) -> Vec<f64>) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    one(kernel(data, us(period)?))
}

/// `LRSLOPE.calculate`: needs `period + 1` bars.
fn lrslope(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period + 1, data.len())?;
    // A number, as Python divides by it (Flow passes a fraction through).
    let interval = p.float("interval")?;
    if interval <= 0.0 {
        return Err(TaError::value(format!(
            "Interval must be positive, got {}",
            p.shown("interval")?
        )));
    }
    one(oa_core::lrslope(data, us(period)?, interval))
}

/// `CORREL.calculate`: two independent price series.
fn correlation(a: &Args) -> TaResult {
    let [first, second] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period, first.len())?;
    one(oa_core::correl(first, second, us(period)?))
}

/// `BETA.calculate`: the asset against a benchmark, `period + 1` bars.
fn beta(a: &Args) -> TaResult {
    let [asset, market] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period + 1, asset.len())?;
    one(oa_core::beta(asset, market, us(period)?))
}

/// `VAR.calculate` and `_backend.variance`, with the z-score components when
/// `return_components` is set.
fn variance(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let lookback = p.int("lookback")?;
    validate_period(lookback, data.len())?;
    let mode = p.text("mode")?;
    if mode != "LR" && mode != "PR" {
        return Err(TaError::value(
            "Mode must be 'LR' (Logarithmic Returns) or 'PR' (Price)",
        ));
    }
    let var = oa_core::variance(data, us(lookback)?, mode == "LR");
    if !p.flag("return_components")? {
        return one(var);
    }
    let sd = map(&var, f64::sqrt);
    let ema_var = oa_core::ema(&var, us(p.int("ema_period")?)?);
    let filter = us(p.int("filter_lookback")?)?;
    let var_sma = oa_core::sma(&var, filter);
    let var_sd = oa_core::stdev(&var, filter);
    let zscore: Vec<f64> = (0..var.len())
        .map(|i| {
            let (v, m, s) = (var[i], var_sma[i], var_sd[i]);
            if !v.is_nan() && !m.is_nan() && !s.is_nan() && s > 0.0 {
                (v - m) / s
            } else {
                f64::NAN
            }
        })
        .collect();
    let ema_z = oa_core::ema(&zscore, us(p.int("ema_length")?)?);
    many(vec![var, ema_var, zscore, ema_z, sd])
}

/// `_backend._median_pnr`: the rolling median by sorting each window, the
/// lower middle value for an even window.
fn median_pnr(data: &[f64], period: usize) -> Vec<f64> {
    let n = data.len();
    let mut out = nan_vec(n);
    if period == 0 {
        return out;
    }
    let pick = if period % 2 == 1 {
        period / 2
    } else {
        period / 2 - 1
    };
    for i in period - 1..n {
        out[i] = sorted(&data[i + 1 - period..=i])[pick];
    }
    out
}

/// `MedianBands.calculate_with_bands` and `_backend.median_bands`: the
/// median of the bar midpoint (the agent passes no `source`), ATR bands and
/// an EMA of the median.
fn median_bands(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let source = match a.series.get(3) {
        Some(series) => non_empty(series)?.to_vec(),
        None => zip(high, low, |h, l| (h + l) / 2.0),
    };
    let median_length = us(p.int("median_length")?)?;
    let median = median_pnr(&source, median_length);
    let multiplier = p.float("atr_mult")?;
    let atr = map(
        &oa_core::atr_wilder(high, low, close, us(p.int("atr_length")?)?),
        |t| t * multiplier,
    );
    let ema = oa_core::ema_first_valid(&median, median_length);
    many(vec![
        median.clone(),
        zip(&median, &atr, |m, t| m + t),
        zip(&median, &atr, |m, t| m - t),
        ema,
    ])
}

/// `MODE.calculate`.
fn mode(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, data.len())?;
    one(oa_core::mode(data, us(period)?, us(p.int("bins")?)?))
}
