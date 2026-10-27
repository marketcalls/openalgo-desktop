//! `openalgo/indicators/oscillators.py` (the 19 oscillator methods) and the
//! `_backend.py` compositions they call.

use super::numpy::{clip, map, minimum, nan_vec, roll, shift, sum, zip};
use super::{many, non_empty, one, us, validate_period, Args, TaError, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "cmo" => cmo(a),
        "trix" => trix(a),
        "uo_oscillator" => uo_oscillator(a),
        "awesome_oscillator" => awesome_oscillator(a),
        "accelerator_oscillator" => accelerator_oscillator(a),
        "ppo" => ppo(a),
        "po" => po(a),
        "dpo" => dpo(a),
        "aroon_oscillator" => aroon_oscillator(a),
        "stochrsi" => stochrsi(a),
        "rvi" => rvi(a),
        "cho" => cho(a),
        "chop" => chop(a),
        "kst" => kst(a),
        "tsi" => tsi(a),
        "vi" => vi(a),
        "stc" => stc(a),
        "gator_oscillator" => gator_oscillator(a),
        "coppock" => coppock(a),
        _ => return None,
    })
}

/// `CMO.calculate`: needs `period + 1` bars.
fn cmo(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = a.p().int("period")?;
    validate_period(period + 1, data.len())?;
    one(oa_core::cmo(data, us(period)?))
}

/// `TRIX.calculate` and `_backend.trix`: the one-bar change of a triple EMA
/// of the log price, times 10000.
fn trix(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let length = a.p().int("length")?;
    validate_period(length, data.len())?;
    let length = us(length)?;
    let logs = map(data, f64::ln);
    let e = oa_core::ema(&oa_core::ema(&oa_core::ema(&logs, length), length), length);
    let mut out = nan_vec(data.len());
    for i in 1..data.len() {
        out[i] = (e[i] - e[i - 1]) * 10000.0;
    }
    one(out)
}

/// `UO.calculate` and `_backend.uo`: the oscillators module's own Ultimate
/// Oscillator, a NumPy composition rather than the volatility module's
/// kernel. A window with no range scores 0 rather than NaN.
fn uo_oscillator(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let n = close.len();
    let mut true_low = vec![0.0; n];
    if n > 0 {
        true_low[0] = low[0];
    }
    for i in 1..n {
        true_low[i] = minimum(low[i], close[i - 1]);
    }
    let tr = oa_core::true_range(high, low, close);
    let buying = zip(close, &true_low, |c, t| c - t);
    let average = |period: i64| -> Result<Vec<f64>, TaError> {
        let period = us(period)?;
        let bp = roll(&buying, period, sum);
        let trs = roll(&tr, period, sum);
        Ok(zip(&bp, &trs, |b, t| if t > 0.0 { b / t } else { 0.0 }))
    };
    let (p1, p2, p3) = (p.int("period1")?, p.int("period2")?, p.int("period3")?);
    let (a1, a2, a3) = (average(p1)?, average(p2)?, average(p3)?);
    let mut out: Vec<f64> = (0..n)
        .map(|i| 100.0 * (4.0 * a1[i] + 2.0 * a2[i] + a3[i]) / 7.0)
        .collect();
    let warmup = (p1.max(p2).max(p3) - 1).max(0) as usize;
    for value in out.iter_mut().take(warmup) {
        *value = f64::NAN;
    }
    one(out)
}

/// `_backend.ao`: fast minus slow mean of the bar midpoint.
fn ao(high: &[f64], low: &[f64], fast: usize, slow: usize) -> Vec<f64> {
    let mid = zip(high, low, |h, l| (h + l) / 2.0);
    zip(
        &oa_core::win_mean(&mid, fast),
        &oa_core::win_mean(&mid, slow),
        |f, s| f - s,
    )
}

/// `AO.calculate`.
fn awesome_oscillator(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let p = a.p();
    one(ao(
        high,
        low,
        us(p.int("fast_period")?)?,
        us(p.int("slow_period")?)?,
    ))
}

/// `AC.calculate` and `_backend.ac`: the 5/34 Awesome Oscillator minus its
/// own mean (the periods are fixed, as on the web).
fn accelerator_oscillator(a: &Args) -> TaResult {
    let high = non_empty(a.s(0)?)?;
    let low = non_empty(a.s(1)?)?;
    let awesome = ao(high, low, 5, 34);
    let average = oa_core::win_mean(&awesome, us(a.p().int("period")?)?);
    one(zip(&awesome, &average, |x, m| x - m))
}

/// `PPO.calculate` and `_backend.ppo`: 0 where the slow EMA is 0.
fn ppo(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let fast = oa_core::ema(data, us(p.int("fast_period")?)?);
    let slow = oa_core::ema(data, us(p.int("slow_period")?)?);
    let line = zip(&fast, &slow, |f, s| {
        if s != 0.0 {
            (f - s) / s * 100.0
        } else {
            0.0
        }
    });
    let signal = oa_core::ema(&line, us(p.int("signal_period")?)?);
    let histogram = zip(&line, &signal, |l, s| l - s);
    many(vec![line, signal, histogram])
}

/// `PO.calculate` and `_backend.price_oscillator`.
fn po(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let fast = us(p.int("fast_period")?)?;
    let slow = us(p.int("slow_period")?)?;
    let ma_type = p.text("ma_type")?;
    let (fast_ma, slow_ma) = match ma_type.to_uppercase().as_str() {
        "SMA" => (oa_core::win_mean(data, fast), oa_core::win_mean(data, slow)),
        "EMA" => (oa_core::ema(data, fast), oa_core::ema(data, slow)),
        _ => return Err(TaError::value(format!("Unsupported MA type: {}", ma_type))),
    };
    one(zip(&fast_ma, &slow_ma, |f, s| f - s))
}

/// `DPO.calculate` and `_backend.dpo`: price against a mean displaced by
/// `period / 2 + 1` bars.
fn dpo(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let period = p.int("period")?;
    validate_period(period, data.len())?;
    let n = data.len();
    let mean = oa_core::win_mean(data, us(period)?);
    let barsback = (period as f64 / 2.0 + 1.0) as usize;
    let mut out = nan_vec(n);
    if barsback >= n {
        return one(out);
    }
    let centered = p.flag("is_centered")?;
    for j in 0..n - barsback {
        let (price, avg) = if centered {
            (data[j], mean[barsback + j])
        } else {
            (data[barsback + j], mean[j])
        };
        out[barsback + j] = if avg.is_nan() { f64::NAN } else { price - avg };
    }
    one(out)
}

/// `AROONOSC.calculate` and `_backend.aroon_osc`.
fn aroon_oscillator(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period, high.len())?;
    let (up, down) = oa_core::aroon(high, low, us(period)?);
    one(zip(&up, &down, |u, d| u - d))
}

/// `StochRSI.calculate`: needs `rsi_period + stoch_period` bars.
fn stochrsi(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let rsi_period = p.int("rsi_period")?;
    let stoch_period = p.int("stoch_period")?;
    validate_period(rsi_period + stoch_period, data.len())?;
    let (k, d) = oa_core::stochrsi(
        data,
        us(rsi_period)?,
        us(stoch_period)?,
        us(p.int("k_period")?)?,
        us(p.int("d_period")?)?,
    );
    many(vec![k, d])
}

/// `RVI.calculate` (the Relative Vigor Index).
fn rvi(a: &Args) -> TaResult {
    let [open, high, low, close] = a.aligned::<4>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    let (line, signal) = oa_core::rvi_vigor(open, high, low, close, us(period)?);
    many(vec![line, signal])
}

/// `CHO.calculate`: the Chaikin oscillator kernel (`adosc`).
fn cho(a: &Args) -> TaResult {
    let [high, low, close, volume] = a.aligned::<4>()?;
    let p = a.p();
    one(oa_core::adosc(
        high,
        low,
        close,
        volume,
        us(p.int("fast_period")?)?,
        us(p.int("slow_period")?)?,
    ))
}

/// `CHOP.calculate` and `_backend.chop`: 50 where the window has no range.
fn chop(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    let length = us(period)?;
    let atr_sum = oa_core::rolling_sum(&oa_core::true_range(high, low, close), length);
    let range = zip(
        &oa_core::highest(high, length),
        &oa_core::lowest(low, length),
        |h, l| h - l,
    );
    let log_period = (period as f64).log10();
    one((0..close.len())
        .map(|i| {
            let (s, r) = (atr_sum[i], range[i]);
            let valid = r > 0.0 && s > 0.0 && !r.is_nan() && !s.is_nan();
            if valid {
                if period > 1 {
                    100.0 * (s / r).log10() / log_period
                } else {
                    f64::NAN
                }
            } else if !s.is_nan() {
                50.0
            } else {
                f64::NAN
            }
        })
        .collect())
}

/// `_backend.roc_osc`: percent change over `period` bars, 0 from a zero base.
fn roc_osc(data: &[f64], period: usize) -> Vec<f64> {
    let n = data.len();
    let mut out = nan_vec(n);
    if period >= n {
        return out;
    }
    for i in period..n {
        let prev = data[i - period];
        out[i] = if prev != 0.0 {
            (data[i] - prev) / prev * 100.0
        } else {
            0.0
        };
    }
    out
}

/// `KST.calculate` and `_backend.kst`.
fn kst(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let names = [
        "roclen1", "roclen2", "roclen3", "roclen4", "smalen1", "smalen2", "smalen3", "smalen4",
        "siglen",
    ];
    let mut values = [0i64; 9];
    for (slot, name) in values.iter_mut().zip(names) {
        *slot = p.int(name)?;
        if *slot <= 0 {
            return Err(TaError::value(format!(
                "{} must be positive, got {}",
                name, slot
            )));
        }
    }
    let smoothed = |roc_len: i64, sma_len: i64| -> Result<Vec<f64>, TaError> {
        Ok(oa_core::win_mean(
            &roc_osc(data, us(roc_len)?),
            us(sma_len)?,
        ))
    };
    let sm1 = smoothed(values[0], values[4])?;
    let sm2 = smoothed(values[1], values[5])?;
    let sm3 = smoothed(values[2], values[6])?;
    let sm4 = smoothed(values[3], values[7])?;
    let line: Vec<f64> = (0..data.len())
        .map(|i| sm1[i] * 1.0 + sm2[i] * 2.0 + sm3[i] * 3.0 + sm4[i] * 4.0)
        .collect();
    let signal = oa_core::win_mean(&line, us(values[8])?);
    many(vec![line, signal])
}

/// `TSI.calculate` and `_backend.tsi`: double-smoothed momentum over
/// double-smoothed absolute momentum.
fn tsi(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let long = us(p.int("long_period")?)?;
    let short = us(p.int("short_period")?)?;
    let mut change = vec![0.0; data.len()];
    for i in 1..data.len() {
        change[i] = data[i] - data[i - 1];
    }
    let absolute = map(&change, f64::abs);
    let num = oa_core::ema(&oa_core::ema(&change, long), short);
    let den = oa_core::ema(&oa_core::ema(&absolute, long), short);
    let line = zip(
        &num,
        &den,
        |x, y| if y != 0.0 { 100.0 * (x / y) } else { 0.0 },
    );
    let signal = oa_core::ema(&line, us(p.int("signal_period")?)?);
    many(vec![line, signal])
}

/// `VI.calculate` and `_backend.vi`. Enabled on the desktop although the web
/// refuses it: the web's "broken" verdict was measured on openalgo 2.0.3,
/// and the 2.0.5 build it pins computes it.
fn vi(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let period = a.p().int("period")?;
    validate_period(period + 1, close.len())?;
    let length = us(period)?;
    let n = close.len();
    let mut plus_move = nan_vec(n);
    let mut minus_move = nan_vec(n);
    for i in 1..n {
        plus_move[i] = (high[i] - low[i - 1]).abs();
        minus_move[i] = (low[i] - high[i - 1]).abs();
    }
    let plus_sum = oa_core::rolling_sum(&plus_move, length);
    let minus_sum = oa_core::rolling_sum(&minus_move, length);
    let range_sum = oa_core::rolling_sum(&oa_core::true_range(high, low, close), length);
    let mut plus = nan_vec(n);
    let mut minus = nan_vec(n);
    for i in length..n {
        let total = range_sum[i];
        let ok = total > 0.0 && !total.is_nan();
        plus[i] = if ok { plus_sum[i] / total } else { 0.0 };
        minus[i] = if ok { minus_sum[i] / total } else { 0.0 };
    }
    many(vec![plus, minus])
}

/// `STC.calculate` and `_backend.stc`: the MACD through two stochastic
/// passes, clipped to 0..100.
fn stc(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let cycle = us(p.int("cycle_length")?)?;
    let macd = zip(
        &oa_core::ema(data, us(p.int("fast_length")?)?),
        &oa_core::ema(data, us(p.int("slow_length")?)?),
        |f, s| f - s,
    );
    let zero_nan = |v: Vec<f64>| map(&v, |x| if x.is_nan() { 0.0 } else { x });
    let k = zero_nan(oa_core::stoch_single(&macd, cycle));
    let d = oa_core::ema(&k, us(p.int("d1_length")?)?);
    let kd = zero_nan(oa_core::stoch_single(&d, cycle));
    let s = oa_core::ema(&kd, us(p.int("d2_length")?)?);
    one(map(&s, |x| clip(x, 0.0, 100.0)))
}

/// `GatorOscillator.calculate` and `_backend.gator`: the Alligator's lines on
/// the bar midpoint, with the shifts fixed at 8, 5 and 3.
fn gator_oscillator(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let p = a.p();
    let mid = zip(high, low, |h, l| (h + l) / 2.0);
    let line = |period: &str, offset: i64| -> Result<Vec<f64>, TaError> {
        Ok(shift(
            &oa_core::ema_wilder(&mid, us(p.int(period)?)?),
            offset,
        ))
    };
    let jaw = line("jaw_period", 8)?;
    let teeth = line("teeth_period", 5)?;
    let lips = line("lips_period", 3)?;
    many(vec![
        zip(&jaw, &teeth, |j, t| (j - t).abs()),
        zip(&teeth, &lips, |t, l| -(t - l).abs()),
    ])
}

/// `Coppock.calculate` and `_backend.coppock`.
fn coppock(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let rates = zip(
        &oa_core::roc(data, us(p.int("long_roc_length")?)?),
        &oa_core::roc(data, us(p.int("short_roc_length")?)?),
        |l, s| l + s,
    );
    one(oa_core::wma_nan(&rates, us(p.int("wma_length")?)?))
}
