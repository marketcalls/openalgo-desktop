//! `openalgo/indicators/trend.py` (the 20 trend methods) and the
//! `_backend.py` compositions they call.

use super::numpy::{mean, nan_vec, roll, shift};
use super::{many, non_empty, one, us, validate_period, Args, TaError, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "sma" => single(a, oa_core::sma),
        "ema" => single(a, oa_core::ema),
        "wma" => single(a, oa_core::wma),
        "hma" => single(a, oa_core::hma),
        "trima" => single(a, oa_core::trima),
        "mcginley" => single(a, oa_core::mcginley),
        "dema" => dema(a),
        "tema" => tema(a),
        "vwma" => vwma(a),
        "alma" => alma(a),
        "kama" => kama(a),
        "zlema" => zlema(a),
        "t3" => t3(a),
        "frama" => frama(a),
        "supertrend" => supertrend(a),
        "ichimoku" => ichimoku(a),
        "ckstop" => ckstop(a),
        "vidya" => vidya(a),
        "alligator" => alligator(a),
        "ma_envelopes" => ma_envelopes(a),
        _ => return None,
    })
}

/// SMA, EMA, WMA, HMA, TRIMA, McGinley: one series, `validate_period`, one
/// kernel.
fn single(a: &Args, kernel: fn(&[f64], usize) -> Vec<f64>) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    one(kernel(data, us(period)?))
}

/// The second EMA pass of DEMA and TEMA: an EMA of `ema1` from its first
/// value on, placed back from `first_valid + period - 1`.
fn second_pass(ema1: &[f64], period: usize) -> Vec<f64> {
    let n = ema1.len();
    let first_valid = period - 1;
    let partial = oa_core::ema(&ema1[first_valid..], period);
    let start = first_valid + period - 1;
    let mut ema2 = nan_vec(n);
    if start < n {
        let tail = partial.len().saturating_sub(period - 1);
        let len = tail.min(n - start);
        ema2[start..start + len].copy_from_slice(&partial[period - 1..period - 1 + len]);
    }
    ema2
}

/// `DEMA.calculate`.
fn dema(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    let period = us(period)?;
    let ema1 = oa_core::ema(data, period);
    let ema2 = second_pass(&ema1, period);
    one(ema1
        .iter()
        .zip(&ema2)
        .map(|(e1, e2)| 2.0 * e1 - e2)
        .collect())
}

/// `TEMA.calculate`: the third pass runs over the second's finite values.
fn tema(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    let period = us(period)?;
    let n = data.len();
    let ema1 = oa_core::ema(data, period);
    let ema2 = second_pass(&ema1, period);
    let second_start = period - 1 + period - 1;
    let mut ema3 = nan_vec(n);
    if second_start < n {
        let clean: Vec<f64> = ema2[second_start..]
            .iter()
            .copied()
            .filter(|x| !x.is_nan())
            .collect();
        if clean.len() >= period {
            let partial = oa_core::ema(&clean, period);
            let third_start = second_start + period - 1;
            if third_start < n {
                let tail = partial.len().saturating_sub(period - 1);
                let len = tail.min(n - third_start);
                ema3[third_start..third_start + len]
                    .copy_from_slice(&partial[period - 1..period - 1 + len]);
            }
        }
    }
    one((0..n)
        .map(|i| 3.0 * ema1[i] - 3.0 * ema2[i] + ema3[i])
        .collect())
}

/// `VWMA.calculate`.
fn vwma(a: &Args) -> TaResult {
    let [data, volume] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    one(oa_core::vwma(data, volume, us(period)?))
}

/// `ALMA.calculate`.
fn alma(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, data.len())?;
    let offset = p.float("offset")?;
    if !(0.0..=1.0).contains(&offset) {
        return Err(TaError::value(format!(
            "Offset must be between 0 and 1, got {}",
            p.shown("offset")?
        )));
    }
    let sigma = p.float("sigma")?;
    if sigma <= 0.0 {
        return Err(TaError::value(format!(
            "Sigma must be positive, got {}",
            p.shown("sigma")?
        )));
    }
    one(oa_core::alma(data, us(period)?, offset, sigma))
}

/// `KAMA.calculate` (the TradingView form, `kama_tv`).
fn kama(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let length = p.int("length")?;
    validate_period(length + 1, data.len())?;
    let fast = p.int("fast_length")?;
    let slow = p.int("slow_length")?;
    if length <= 0 {
        return Err(TaError::value(format!(
            "Length must be positive, got {}",
            length
        )));
    }
    if fast <= 0 {
        return Err(TaError::value(format!(
            "Fast length must be positive, got {}",
            fast
        )));
    }
    if slow <= 0 {
        return Err(TaError::value(format!(
            "Slow length must be positive, got {}",
            slow
        )));
    }
    if fast >= slow {
        return Err(TaError::value("Fast length must be less than slow length"));
    }
    one(oa_core::kama_tv(data, us(length)?, us(fast)?, us(slow)?))
}

/// `ZLEMA.calculate`: an EMA of `2 * x[i] - x[i - lag]`.
fn zlema(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    let lag = ((period - 1) / 2) as usize;
    let mut adjusted = data.to_vec();
    if lag > 0 {
        for i in lag..data.len() {
            adjusted[i] = 2.0 * data[i] - data[i - lag];
        }
    }
    one(oa_core::ema(&adjusted, us(period)?))
}

/// `T3.calculate`.
fn t3(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period, data.len())?;
    one(oa_core::t3(data, us(period)?, a.p().float("v_factor")?))
}

/// `FRAMA.calculate`: high and low, an even period of at least 2.
fn frama(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period, high.len())?;
    if period < 2 {
        return Err(TaError::value(
            "Period must be at least 2 for FRAMA calculation",
        ));
    }
    if period % 2 != 0 {
        return Err(TaError::value("Period must be even for FRAMA calculation"));
    }
    one(oa_core::frama(high, low, us(period)?))
}

/// `Supertrend.calculate`.
fn supertrend(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, close.len())?;
    let multiplier = p.float("multiplier")?;
    if multiplier <= 0.0 {
        return Err(TaError::value(format!(
            "Multiplier must be positive, got {}",
            p.shown("multiplier")?
        )));
    }
    let (line, direction) = oa_core::supertrend(high, low, close, us(period)?, multiplier);
    many(vec![line, direction])
}

/// `Ichimoku.calculate` and `_backend.ichimoku`.
fn ichimoku(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let conversion = p.int("conversion_periods")?;
    let base = p.int("base_periods")?;
    let span2 = p.int("lagging_span2_periods")?;
    for (period, name) in [
        (conversion, "conversion_periods"),
        (base, "base_periods"),
        (span2, "lagging_span2_periods"),
    ] {
        if period <= 0 {
            return Err(TaError::value(format!(
                "{} must be positive, got {}",
                name, period
            )));
        }
    }
    let displacement = p.int("displacement")?;
    let donchian = |period: i64| -> Result<Vec<f64>, TaError> {
        let period = us(period)?;
        let upper = oa_core::highest(high, period);
        let lower = oa_core::lowest(low, period);
        Ok(upper
            .iter()
            .zip(&lower)
            .map(|(u, l)| (u + l) / 2.0)
            .collect())
    };
    let conversion_line = donchian(conversion)?;
    let base_line = donchian(base)?;
    let lead1: Vec<f64> = conversion_line
        .iter()
        .zip(&base_line)
        .map(|(c, b)| (c + b) / 2.0)
        .collect();
    let lead2 = donchian(span2)?;
    // The leading spans move `displacement - 1` bars forward; the lagging
    // span moves the close the same distance back.
    let offset = displacement - 1;
    many(vec![
        conversion_line,
        base_line,
        shift(&lead1, offset),
        shift(&lead2, offset),
        lagging(close, offset),
    ])
}

/// Ichimoku's lagging span: the close `offset` bars back (`_backend.ichimoku`).
fn lagging(close: &[f64], offset: i64) -> Vec<f64> {
    let n = close.len();
    if offset > 0 {
        let mut out = nan_vec(n);
        let k = offset as usize;
        if k < n {
            out[..n - k].copy_from_slice(&close[k..]);
        }
        out
    } else {
        shift(close, -offset)
    }
}

/// `ChandeKrollStop.calculate`.
fn ckstop(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let atr_period = p.int("p")?;
    let stop_period = p.int("q")?;
    validate_period(atr_period, close.len())?;
    validate_period(stop_period, close.len())?;
    let x = p.float("x")?;
    if x <= 0.0 {
        return Err(TaError::value(format!(
            "ATR coefficient (x) must be positive, got {}",
            p.shown("x")?
        )));
    }
    let (long, short) =
        oa_core::chande_kroll_stop(high, low, close, us(atr_period)?, x, us(stop_period)?);
    many(vec![long, short])
}

/// `VIDYA.calculate`.
fn vidya(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period + 1, data.len())?;
    one(oa_core::vidya(data, us(period)?, a.p().float("alpha")?))
}

/// `Alligator.calculate`: three Wilder averages, each shifted forward.
fn alligator(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let line = |period: &str, offset: &str| -> Result<Vec<f64>, TaError> {
        let smoothed = oa_core::ema_wilder(data, us(p.int(period)?)?);
        Ok(shift(&smoothed, p.int(offset)?))
    };
    many(vec![
        line("jaw_period", "jaw_shift")?,
        line("teeth_period", "teeth_shift")?,
        line("lips_period", "lips_shift")?,
    ])
}

/// `MovingAverageEnvelopes.calculate` and `_backend.ma_envelopes`.
fn ma_envelopes(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, data.len())?;
    let ma_type = p.text("ma_type")?;
    let period = us(period)?;
    let ma = match ma_type.to_uppercase().as_str() {
        "SMA" => roll(data, period, mean),
        "EMA" => oa_core::ema(data, period),
        _ => return Err(TaError::value(format!("Unsupported MA type: {}", ma_type))),
    };
    let mult = p.float("percentage")? / 100.0;
    many(vec![
        ma.iter().map(|m| m * (1.0 + mult)).collect(),
        ma.clone(),
        ma.iter().map(|m| m * (1.0 - mult)).collect(),
    ])
}
