//! `openalgo/indicators/momentum.py` (the 9 momentum methods) and the
//! `_backend.py` compositions they call.

use super::numpy::nan_vec;
use super::{many, non_empty, one, us, validate_period, Args, TaError, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "rsi" => rsi(a),
        "macd" => macd(a),
        "stochastic" => stochastic(a),
        "cci" => hlc_period(a, oa_core::cci),
        "williams_r" => hlc_period(a, oa_core::williams_r),
        "bop" => bop(a),
        "elderray" => elderray(a),
        "fisher" => fisher(a),
        "crsi" => crsi(a),
        _ => return None,
    })
}

/// `RSI.calculate`.
fn rsi(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    one(oa_core::rsi(data, us(period)?))
}

/// `MACD.calculate` and `_backend.macd`: EMAs seeded from the first bar.
fn macd(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let fast = p.int("fast_period")?;
    let slow = p.int("slow_period")?;
    let signal = p.int("signal_period")?;
    if fast <= 0 || slow <= 0 || signal <= 0 {
        return Err(TaError::value("All periods must be positive"));
    }
    if fast >= slow {
        return Err(TaError::value("Fast period must be less than slow period"));
    }
    let fast_ema = oa_core::ema(data, us(fast)?);
    let slow_ema = oa_core::ema(data, us(slow)?);
    let line: Vec<f64> = fast_ema.iter().zip(&slow_ema).map(|(f, s)| f - s).collect();
    let signal_line = oa_core::ema(&line, us(signal)?);
    let histogram = line.iter().zip(&signal_line).map(|(m, s)| m - s).collect();
    many(vec![line, signal_line, histogram])
}

/// `Stochastic.calculate`: `k_period`, `smooth_k`, `d_period` in that order.
fn stochastic(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let k_period = p.int("k_period")?;
    validate_period(k_period, close.len())?;
    let smooth_k = p.int("smooth_k")?;
    let d_period = p.int("d_period")?;
    if d_period <= 0 {
        return Err(TaError::value(format!(
            "d_period must be positive, got {}",
            d_period
        )));
    }
    if smooth_k <= 0 {
        return Err(TaError::value(format!(
            "smooth_k must be positive, got {}",
            smooth_k
        )));
    }
    let (k, d) = oa_core::stochastic(
        high,
        low,
        close,
        us(k_period)?,
        us(smooth_k)?,
        us(d_period)?,
    );
    many(vec![k, d])
}

/// CCI and Williams %R: high, low, close and a validated period.
fn hlc_period(a: &Args, kernel: fn(&[f64], &[f64], &[f64], usize) -> Vec<f64>) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    one(kernel(high, low, close, us(period)?))
}

/// `BOP.calculate`.
fn bop(a: &Args) -> TaResult {
    let [open, high, low, close] = a.aligned::<4>()?;
    one(oa_core::bop(open, high, low, close))
}

/// `ElderRay.calculate` and `_backend.elderray`: no period check.
fn elderray(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let ema = oa_core::ema(close, us(a.p().int("period")?)?);
    many(vec![
        high.iter().zip(&ema).map(|(h, e)| h - e).collect(),
        low.iter().zip(&ema).map(|(l, e)| l - e).collect(),
    ])
}

/// `Fisher.calculate`: the transform of the bar midpoint.
fn fisher(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let length = a.p().int("length")?;
    validate_period(length, high.len())?;
    let hl2: Vec<f64> = high.iter().zip(low).map(|(h, l)| (h + l) / 2.0).collect();
    let (fisher, trigger) = oa_core::fisher(&hl2, us(length)?);
    many(vec![fisher, trigger])
}

/// `CRSI.calculate` and `_backend.crsi`: the mean of the price RSI, the
/// streak RSI and the percent rank of the one-bar rate of change.
fn crsi(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let lenrsi = p.int("lenrsi")?;
    let lenupdown = p.int("lenupdown")?;
    let lenroc = p.int("lenroc")?;
    validate_period(lenrsi.max(lenupdown).max(lenroc), data.len())?;
    let price_rsi = oa_core::rsi(data, us(lenrsi)?);
    let streak_rsi = oa_core::rsi(&oa_core::updown_streak(data), us(lenupdown)?);
    let rank = oa_core::percent_rank(&oa_core::roc(data, 1), us(lenroc)?);
    let mut out = nan_vec(data.len());
    for i in 0..data.len() {
        let (r, s, k) = (price_rsi[i], streak_rsi[i], rank[i]);
        if !(r.is_nan() || s.is_nan() || k.is_nan()) {
            out[i] = (r + s + k) / 3.0;
        }
    }
    one(out)
}
