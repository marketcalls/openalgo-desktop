//! `openalgo/indicators/volume.py` (the 17 volume methods) and the
//! `_backend.py` compositions they call.

use super::numpy::{cumsum, mean, nan_vec, nansum, sum, zip};
use super::volatility::bbands_checked;
use super::{many, non_empty, one, us, validate_period, Args, HlcvKernel, TaError, TaResult};

const NO_VOLUME: &str = "No volume is provided by the data vendor.";

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "obv" => obv(a),
        "obv_smoothed" => obv_smoothed(a),
        "vwap" => vwap(a),
        "mfi" => hlcv_period(a, oa_core::mfi),
        "adl" => adl(a),
        "cmf" => hlcv_period(a, oa_core::cmf),
        "emv" => emv(a),
        "force_index" => force_index(a),
        "nvi" => nvi_only(a),
        "nvi_with_ema" => nvi_with_ema(a),
        "pvi" => pvi(a),
        "pvi_with_signal" => pvi_with_signal(a),
        "volosc" => volosc(a),
        "vroc" => vroc(a),
        "kvo" => kvo(a),
        "pvt" => pvt(a),
        "rvol" => rvol(a),
        _ => return None,
    })
}

/// `OBV.calculate`.
fn obv(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    one(oa_core::obv(close, volume))
}

/// `OBVSmoothed.calculate`: OBV, optionally smoothed. The Bollinger mode
/// returns `BollingerBands.calculate`'s (upper, middle, lower) as is.
fn obv_smoothed(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    let p = a.p();
    let obv = oa_core::obv(close, volume);
    let ma_type = p.text("ma_type")?;
    let checked = |kernel: fn(&[f64], usize) -> Vec<f64>| -> TaResult {
        let length = p.int("ma_length")?;
        validate_period(length, obv.len())?;
        one(kernel(&obv, us(length)?))
    };
    match ma_type {
        "None" => one(obv.clone()),
        "SMA" => checked(oa_core::sma),
        "SMA + Bollinger Bands" => bbands_checked(&obv, a, "bb_length", "bb_mult"),
        "EMA" => checked(oa_core::ema),
        "SMMA (RMA)" => one(rma_smma(&obv, us(p.int("ma_length")?)?)),
        "WMA" => checked(oa_core::wma),
        "VWMA" => one(vwma_strict(&obv, volume, p.int("ma_length")?)),
        other => Err(TaError::value(format!("Unsupported ma_type: {}", other))),
    }
}

/// `_backend._first_clean_window`.
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

/// `_backend.rma_smma`: seeded by the mean of the first clean window.
fn rma_smma(data: &[f64], length: usize) -> Vec<f64> {
    let n = data.len();
    let mut out = nan_vec(n);
    let Some(first) = first_clean_window(data, length) else {
        return out;
    };
    let mut prev = sum(&data[first..first + length]) / length as f64;
    out[first + length - 1] = prev;
    let alpha = 1.0 / length as f64;
    for i in first + length..n {
        if !data[i].is_nan() {
            prev = alpha * data[i] + (1.0 - alpha) * prev;
            out[i] = prev;
        }
    }
    out
}

/// `_backend.vwma_strict`: bars with a missing value or volume are skipped.
fn vwma_strict(values: &[f64], volume: &[f64], length: i64) -> Vec<f64> {
    let n = values.len();
    let mut out = nan_vec(n);
    let Ok(length) = usize::try_from(length) else {
        return out;
    };
    if length == 0 {
        return out;
    }
    for (i, slot) in out.iter_mut().enumerate().skip(length - 1) {
        let window = i + 1 - length..=i;
        let (mut weighted, mut total) = (0.0, 0.0);
        for (v, vol) in values[window.clone()].iter().zip(&volume[window]) {
            if !v.is_nan() && !vol.is_nan() {
                weighted += v * vol;
                total += vol;
            }
        }
        if total > 0.0 {
            *slot = weighted / total;
        }
    }
    out
}

/// `VWAP.calculate`: one session from the first bar (the agent passes no
/// timestamps), on the chosen source; the band arguments are accepted and
/// unused, as on the web.
fn vwap(a: &Args) -> TaResult {
    let [high, low, close, volume] = a.aligned::<4>()?;
    if sum(volume) == 0.0 {
        return Err(TaError::runtime(NO_VOLUME));
    }
    let hlc3 = || -> Vec<f64> {
        (0..close.len())
            .map(|i| (high[i] + low[i] + close[i]) / 3.0)
            .collect()
    };
    let source = match a.p().text("source")? {
        "hl2" => zip(high, low, |h, l| (h + l) / 2.0),
        "close" => close.to_vec(),
        _ => hlc3(),
    };
    let mut starts = vec![0.0; source.len()];
    starts[0] = 1.0;
    let (vwap, _) = oa_core::session_vwap(&source, volume, &starts);
    one(vwap)
}

/// MFI and CMF: high, low, close, volume and a validated period.
fn hlcv_period(a: &Args, kernel: HlcvKernel) -> TaResult {
    let [high, low, close, volume] = a.aligned::<4>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    one(kernel(high, low, close, volume, us(period)?))
}

/// `ADL.calculate`.
fn adl(a: &Args) -> TaResult {
    let [high, low, close, volume] = a.aligned::<4>()?;
    one(oa_core::adl(high, low, close, volume))
}

/// `EMV.calculate` and `_backend.emv`: the raw ease of movement, averaged.
fn emv(a: &Args) -> TaResult {
    let [high, low, volume] = a.aligned::<3>()?;
    let p = a.p();
    let length = p.int("length")?;
    validate_period(length, high.len())?;
    let divisor = p.float("divisor")?;
    if divisor <= 0.0 {
        return Err(TaError::value(format!(
            "Divisor must be positive, got {}",
            p.shown("divisor")?
        )));
    }
    let raw = oa_core::emv_raw(high, low, volume, divisor);
    one(oa_core::win_mean(&raw, us(length)?))
}

/// `FI.calculate` and `_backend.force_index`.
fn force_index(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    let length = a.p().int("length")?;
    validate_period(length, close.len())?;
    if nansum(volume) == 0.0 {
        return Err(TaError::value(NO_VOLUME));
    }
    let mut raw = nan_vec(close.len());
    for i in 1..close.len() {
        raw[i] = volume[i] * (close[i] - close[i - 1]);
    }
    one(oa_core::ema_first_valid(&raw, us(length)?))
}

/// `_backend.nvi`: the running sum of percentage moves on lower volume.
fn nvi(close: &[f64], volume: &[f64]) -> Vec<f64> {
    let n = close.len();
    let added: Vec<f64> = (1..n)
        .map(|i| {
            let prev = close[i - 1];
            let change = if prev != 0.0 {
                (close[i] - prev) / prev * 100.0
            } else {
                0.0
            };
            if volume[i] < volume[i - 1] {
                change
            } else {
                0.0
            }
        })
        .collect();
    let mut out = vec![0.0; n];
    out[1..].copy_from_slice(&cumsum(&added));
    out
}

fn nvi_only(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    one(nvi(close, volume))
}

/// `NVI.calculate_with_ema`.
fn nvi_with_ema(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    let line = nvi(close, volume);
    let ema = oa_core::ema_first_valid(&line, us(a.p().int("ema_length")?)?);
    many(vec![line, ema])
}

/// `_backend.pvi`: the running product of price ratios on higher volume.
fn pvi_line(close: &[f64], volume: &[f64], initial: f64) -> Vec<f64> {
    let n = close.len();
    let mut out = vec![initial; n];
    for i in 1..n {
        let prev = close[i - 1];
        let ratio = if volume[i] > volume[i - 1] && prev != 0.0 {
            close[i] / prev
        } else {
            1.0
        };
        out[i] = out[i - 1] * ratio;
    }
    out
}

fn pvi(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    one(pvi_line(close, volume, a.p().float("initial_value")?))
}

/// `PVI.calculate_with_signal`: the PVI (its `_calculate_pvi` loop gives the
/// values `_backend.pvi` gives) and an EMA or SMA of it, both the class's own
/// interpreted loops rather than kernels.
fn pvi_with_signal(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    let p = a.p();
    let line = pvi_line(close, volume, p.float("initial_value")?);
    let signal_type = p.text("signal_type")?;
    // The EMA's smoothing factor takes any number, as Python's does (Flow
    // passes a fraction through); the SMA's window is an integer.
    let signal = match signal_type.to_uppercase().as_str() {
        "EMA" => skip_nan_ema(&line, p.float("signal_length")?),
        "SMA" => valid_mean(&line, p.int("signal_length")?),
        _ => {
            return Err(TaError::value(format!(
                "Invalid signal_type: {}. Must be 'EMA' or 'SMA'",
                signal_type
            )))
        }
    };
    many(vec![line, signal])
}

/// `PVI._calculate_ema`: seeded at the first value, NaN bars carry it.
fn skip_nan_ema(data: &[f64], period: f64) -> Vec<f64> {
    let n = data.len();
    let mut out = nan_vec(n);
    let alpha = 2.0 / (period + 1.0);
    let Some(first) = data.iter().position(|x| !x.is_nan()) else {
        return out;
    };
    out[first] = data[first];
    for i in first + 1..n {
        out[i] = if data[i].is_nan() {
            out[i - 1]
        } else {
            alpha * data[i] + (1.0 - alpha) * out[i - 1]
        };
    }
    out
}

/// `PVI._calculate_sma`: the mean of each window's finite values.
fn valid_mean(data: &[f64], period: i64) -> Vec<f64> {
    let n = data.len();
    let mut out = nan_vec(n);
    let Ok(period) = usize::try_from(period) else {
        return out;
    };
    if period == 0 {
        return out;
    }
    for i in period - 1..n {
        let finite: Vec<f64> = data[i + 1 - period..=i]
            .iter()
            .copied()
            .filter(|x| !x.is_nan())
            .collect();
        if !finite.is_empty() {
            out[i] = mean(&finite);
        }
    }
    out
}

/// `VOLOSC.calculate` and `_backend.volosc`.
fn volosc(a: &Args) -> TaResult {
    let volume = non_empty(a.s(0)?)?;
    let p = a.p();
    if p.flag("check_volume_validity")? && nansum(volume) == 0.0 {
        return Err(TaError::runtime(NO_VOLUME));
    }
    let short = oa_core::ema_first_valid(volume, us(p.int("short_length")?)?);
    let long = oa_core::ema_first_valid(volume, us(p.int("long_length")?)?);
    one(zip(&short, &long, |s, l| {
        if !l.is_nan() && l != 0.0 {
            100.0 * (s - l) / l
        } else {
            f64::NAN
        }
    }))
}

/// `VROC.calculate` and `_backend.vroc`.
fn vroc(a: &Args) -> TaResult {
    let volume = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, volume.len())?;
    let period = us(period)?;
    let n = volume.len();
    let mut out = nan_vec(n);
    if period >= n {
        return one(out);
    }
    for i in period..n {
        let prev = volume[i - period];
        out[i] = if prev != 0.0 {
            (volume[i] - prev) / prev * 100.0
        } else {
            0.0
        };
    }
    one(out)
}

/// `KlingerVolumeOscillator.calculate` and `_backend.kvo`.
fn kvo(a: &Args) -> TaResult {
    let [high, low, close, volume] = a.aligned::<4>()?;
    let p = a.p();
    let trig_len = p.int("trig_len")?;
    let fast_x = p.int("fast_x")?;
    let slow_x = p.int("slow_x")?;
    for (value, name) in [
        (trig_len, "trig_len"),
        (fast_x, "fast_x"),
        (slow_x, "slow_x"),
    ] {
        if value <= 0 {
            return Err(TaError::value(format!(
                "{} must be positive, got {}",
                name, value
            )));
        }
    }
    let n = close.len();
    let hlc3: Vec<f64> = (0..n)
        .map(|i| (high[i] + low[i] + close[i]) / 3.0)
        .collect();
    let mut trend = vec![0.0; n];
    if n > 0 {
        trend[0] = volume[0] * 100.0;
    }
    for i in 1..n {
        trend[i] = if hlc3[i] > hlc3[i - 1] {
            volume[i] * 100.0
        } else {
            -volume[i] * 100.0
        };
    }
    let fast = oa_core::ema(&trend, us(fast_x)?);
    let slow = oa_core::ema(&trend, us(slow_x)?);
    let line = zip(&fast, &slow, |f, s| f - s);
    let trigger = oa_core::ema(&line, us(trig_len)?);
    many(vec![line, trigger])
}

/// `PriceVolumeTrend.calculate` and `_backend.pvt`.
fn pvt(a: &Args) -> TaResult {
    let [close, volume] = a.aligned::<2>()?;
    let n = close.len();
    let terms: Vec<f64> = (1..n)
        .map(|i| {
            let prev = close[i - 1];
            if prev != 0.0 {
                (close[i] - prev) / prev * volume[i]
            } else {
                0.0
            }
        })
        .collect();
    let mut out = vec![0.0; n];
    out[1..].copy_from_slice(&cumsum(&terms));
    one(out)
}

/// `RVOL.calculate` and `_backend.rvol`: 1.0 where the average is not
/// positive, NaN in the warm-up.
fn rvol(a: &Args) -> TaResult {
    let volume = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, volume.len())?;
    let average = oa_core::win_mean(volume, us(period)?);
    one(zip(volume, &average, |v, avg| {
        if avg.is_nan() {
            f64::NAN
        } else if avg > 0.0 {
            v / avg
        } else {
            1.0
        }
    }))
}
