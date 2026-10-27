//! `openalgo/indicators/volatility.py` (the 15 volatility methods) and the
//! `_backend.py` compositions they call.

use super::numpy::{nan_vec, zip};
use super::{many, non_empty, one, us, validate_period, Args, TaError, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "atr" => atr(a),
        "bbands" => bbands(a),
        "keltner" => keltner(a),
        "donchian" => donchian(a),
        "chaikin" => chaikin(a),
        "natr" => natr(a),
        "ultimate_oscillator" => ultimate_oscillator(a),
        "true_range" => true_range(a),
        "massindex" => massindex(a),
        "bbpercent" => bbpercent(a),
        "bbwidth" => bbwidth(a),
        "chandelier_exit" => chandelier_exit(a),
        "hv" => hv(a),
        "ulcerindex" => ulcerindex(a),
        "starc" => starc(a),
        _ => return None,
    })
}

/// `ATR.calculate`: Wilder's average.
fn atr(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    one(oa_core::atr_wilder(high, low, close, us(period)?))
}

/// `_backend.bbands`: SMA middle, population standard deviation bands.
pub(super) fn bollinger(data: &[f64], period: usize, std_dev: f64) -> Vec<Vec<f64>> {
    let middle = oa_core::sma(data, period);
    let sd = oa_core::stdev(data, period);
    let upper = middle
        .iter()
        .zip(&sd)
        .map(|(m, s)| m + std_dev * s)
        .collect();
    let lower = middle
        .iter()
        .zip(&sd)
        .map(|(m, s)| m - std_dev * s)
        .collect();
    vec![upper, middle, lower]
}

/// `BollingerBands.calculate`, shared with `obv_smoothed`'s band mode.
pub(super) fn bbands_checked(data: &[f64], a: &Args, period: &str, mult: &str) -> TaResult {
    let p = a.p();
    let length = p.int(period)?;
    validate_period(length, data.len())?;
    let std_dev = p.float(mult)?;
    if std_dev <= 0.0 {
        return Err(TaError::value(format!(
            "Standard deviation multiplier must be positive, got {}",
            p.shown(mult)?
        )));
    }
    many(bollinger(data, us(length)?, std_dev))
}

fn bbands(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    bbands_checked(data, a, "period", "std_dev")
}

/// `Keltner.calculate` and `_backend.keltner`: an SMA-seeded EMA middle.
fn keltner(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let ema_period = p.int("ema_period")?;
    let atr_period = p.int("atr_period")?;
    validate_period(ema_period, close.len())?;
    validate_period(atr_period, close.len())?;
    let multiplier = p.float("multiplier")?;
    if multiplier <= 0.0 {
        return Err(TaError::value(format!(
            "Multiplier must be positive, got {}",
            p.shown("multiplier")?
        )));
    }
    let middle = oa_core::ema_sma(close, us(ema_period)?);
    let atr = oa_core::atr_wilder(high, low, close, us(atr_period)?);
    let upper = middle
        .iter()
        .zip(&atr)
        .map(|(m, t)| m + multiplier * t)
        .collect();
    let lower = middle
        .iter()
        .zip(&atr)
        .map(|(m, t)| m - multiplier * t)
        .collect();
    many(vec![upper, middle, lower])
}

/// `Donchian.calculate` and `_backend.donchian`.
fn donchian(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period, high.len())?;
    let period = us(period)?;
    let upper = oa_core::highest(high, period);
    let lower = oa_core::lowest(low, period);
    let middle = zip(&upper, &lower, |u, l| (u + l) / 2.0);
    many(vec![upper, middle, lower])
}

/// `Chaikin.calculate` (volatility): the rate of change of the averaged range.
fn chaikin(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let p = a.p();
    let range = zip(high, low, |h, l| h - l);
    let averaged = oa_core::ema_sma(&range, us(p.int("ema_period")?)?);
    one(oa_core::roc(&averaged, us(p.int("roc_period")?)?))
}

/// `NATR.calculate`: ATR as a percentage of the close, 0 where the close is 0.
fn natr(a: &Args) -> TaResult {
    let close = non_empty(a.s(2)?)?;
    let (high, low) = (a.s(0)?, a.s(1)?);
    let atr = oa_core::atr_wilder(high, low, close, us(a.p().int("period")?)?);
    one(zip(
        &atr,
        close,
        |t, c| if c != 0.0 { t / c * 100.0 } else { 0.0 },
    ))
}

/// `ULTOSC.calculate`: the kernel; no period checks.
fn ultimate_oscillator(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    one(oa_core::ultosc(
        high,
        low,
        close,
        us(p.int("period1")?)?,
        us(p.int("period2")?)?,
        us(p.int("period3")?)?,
    ))
}

/// `TRANGE.calculate`.
fn true_range(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    one(oa_core::true_range(high, low, close))
}

/// `MASS.calculate` and `_backend.mass`: the sum of the single over the
/// double 9-bar EMA of the range.
fn massindex(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let span = zip(high, low, |h, l| h - l);
    let e1 = oa_core::ema(&span, 9);
    let e2 = oa_core::ema(&e1, 9);
    let ratio = zip(&e1, &e2, |x, y| {
        if y != 0.0 && !x.is_nan() && !y.is_nan() {
            x / y
        } else {
            f64::NAN
        }
    });
    one(oa_core::rolling_sum(&ratio, us(a.p().int("length")?)?))
}

/// `_backend._bb_bands`: windowed mean and standard deviation.
fn window_bands(data: &[f64], period: usize, std_dev: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mean = oa_core::win_mean(data, period);
    let sd = oa_core::win_std(data, period);
    let upper = zip(&mean, &sd, |m, s| m + s * std_dev);
    let lower = zip(&mean, &sd, |m, s| m - s * std_dev);
    (upper, mean, lower)
}

/// `BBPercent.calculate`: 0.5 where the bands meet.
fn bbpercent(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, data.len())?;
    let (upper, _, lower) = window_bands(data, us(period)?, p.float("std_dev")?);
    one((0..data.len())
        .map(|i| {
            if !upper[i].is_nan() && upper[i] == lower[i] {
                0.5
            } else {
                (data[i] - lower[i]) / (upper[i] - lower[i])
            }
        })
        .collect())
}

/// `BBWidth.calculate`: 0 where the middle is 0.
fn bbwidth(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, data.len())?;
    let (upper, middle, lower) = window_bands(data, us(period)?, p.float("std_dev")?);
    one((0..data.len())
        .map(|i| {
            if !middle[i].is_nan() && middle[i] == 0.0 {
                0.0
            } else {
                (upper[i] - lower[i]) / middle[i]
            }
        })
        .collect())
}

/// `ChandelierExit.calculate` and `_backend.chandelier_exit`.
fn chandelier_exit(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, close.len())?;
    let period = us(period)?;
    let multiplier = p.float("multiplier")?;
    let atr = oa_core::win_mean(&oa_core::true_range(high, low, close), period);
    let highest = oa_core::highest(high, period);
    let lowest = oa_core::lowest(low, period);
    many(vec![
        zip(&highest, &atr, |h, t| h - t * multiplier),
        zip(&lowest, &atr, |l, t| l + t * multiplier),
    ])
}

/// `HistoricalVolatility.calculate` and `_backend.hv`: the annualised
/// standard deviation of log returns, in percent.
fn hv(a: &Args) -> TaResult {
    let close = non_empty(a.s(0)?)?;
    let p = a.p();
    let length = p.int("length")?;
    validate_period(length + 1, close.len())?;
    let annual = p.int("annual")?;
    let per = p.int("per")?;
    if length < 1 {
        return Err(TaError::value(format!(
            "Length must be at least 1, got {}",
            length
        )));
    }
    if annual <= 0 {
        return Err(TaError::value(format!(
            "Annual periods must be positive, got {}",
            annual
        )));
    }
    if per <= 0 {
        return Err(TaError::value(format!(
            "Per periods must be positive, got {}",
            per
        )));
    }
    let n = close.len();
    let mut log_returns = nan_vec(n);
    for i in 1..n {
        if close[i - 1] > 0.0 && close[i] > 0.0 {
            log_returns[i] = (close[i] / close[i - 1]).ln();
        }
    }
    let sd = oa_core::win_std(&log_returns, us(length)?);
    let scale = (annual as f64 / per as f64).sqrt();
    one(sd.iter().map(|s| 100.0 * s * scale).collect())
}

/// `UlcerIndex.calculate` and `_backend.ulcerindex`. Enabled on the
/// desktop although the web refuses it: the web's "broken" verdict was
/// measured on openalgo 2.0.3, and the 2.0.5 build it pins computes it.
fn ulcerindex(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let length = p.int("length")?;
    let smooth = p.int("smooth_length")?;
    let signal_length = p.int("signal_length")?;
    validate_period(length, data.len())?;
    validate_period(smooth, data.len())?;
    validate_period(signal_length, data.len())?;
    let highest = oa_core::highest(data, us(length)?);
    let drawdown_sq: Vec<f64> = data
        .iter()
        .zip(&highest)
        .map(|(&x, &h)| {
            let dd = if !h.is_nan() && h != 0.0 {
                100.0 * (x - h) / h
            } else {
                f64::NAN
            };
            dd * dd
        })
        .collect();
    let ulcer: Vec<f64> = oa_core::sma(&drawdown_sq, us(smooth)?)
        .iter()
        .map(|v| v.sqrt())
        .collect();
    if !p.flag("return_signal")? {
        return one(ulcer);
    }
    let signal = if p.text("signal_type")?.to_uppercase() == "SMA" {
        oa_core::sma(&ulcer, us(signal_length)?)
    } else {
        oa_core::ema(&ulcer, us(signal_length)?)
    };
    many(vec![ulcer, signal])
}

/// `STARC.calculate` and `_backend.starc`: no period checks.
fn starc(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let multiplier = p.float("multiplier")?;
    let middle = oa_core::win_mean(close, us(p.int("ma_period")?)?);
    let atr = oa_core::win_mean(
        &oa_core::true_range(high, low, close),
        us(p.int("atr_period")?)?,
    );
    many(vec![
        zip(&middle, &atr, |m, t| m + t * multiplier),
        middle.clone(),
        zip(&middle, &atr, |m, t| m - t * multiplier),
    ])
}
