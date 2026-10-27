//! `openalgo/indicators/talib_extra.py` (the 18 TA-Lib style methods) and
//! the `_backend.py` compositions they call. None of them checks its period:
//! a period longer than the data gives NaN, as on the web.

use super::numpy::{nan_vec, zip};
use super::{many, non_empty, one, us, Args, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "mom" => lagged(a, |x, prev| x - prev),
        "rocp" => lagged(a, |x, prev| (x - prev) / prev),
        "rocr" => lagged(a, |x, prev| x / prev),
        "rocr100" => lagged(a, |x, prev| x / prev * 100.0),
        "midpoint" => single(a, oa_core::midpoint),
        "linregangle" => single(a, oa_core::linreg_angle),
        "linregintercept" => single(a, oa_core::linreg_intercept),
        "apo" => apo(a),
        "medprice" => medprice(a),
        "typprice" => typprice(a),
        "wclprice" => wclprice(a),
        "midprice" => hl_period(a, oa_core::midprice),
        "plus_dm" => hl_period(a, oa_core::plus_dm_talib),
        "minus_dm" => hl_period(a, oa_core::minus_dm_talib),
        "avgprice" => avgprice(a),
        "dx" => hlc_period(a, oa_core::dx_talib),
        "adxr" => hlc_period(a, oa_core::adxr_talib),
        "stochf" => stochf(a),
        _ => return None,
    })
}

/// MOM, ROCP, ROCR, ROCR100: `op(x[i], x[i - period])`.
fn lagged(a: &Args, op: fn(f64, f64) -> f64) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let period = us(a.p().int("period")?)?;
    let n = data.len();
    let mut out = nan_vec(n);
    if period < n {
        for i in period..n {
            out[i] = op(data[i], data[i - period]);
        }
    }
    one(out)
}

/// MIDPOINT, LINEARREG_ANGLE, LINEARREG_INTERCEPT: one series, one kernel.
fn single(a: &Args, kernel: fn(&[f64], usize) -> Vec<f64>) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    one(kernel(data, us(a.p().int("period")?)?))
}

/// `APO.calculate` and `_backend.apo`: EMA when asked, the windowed mean for
/// anything else (the type is not validated, as on the web).
fn apo(a: &Args) -> TaResult {
    let data = non_empty(a.s(0)?)?;
    let p = a.p();
    let fast = us(p.int("fast_period")?)?;
    let slow = us(p.int("slow_period")?)?;
    let (f, s) = if p.text("ma_type")?.to_uppercase() == "EMA" {
        (oa_core::ema(data, fast), oa_core::ema(data, slow))
    } else {
        (oa_core::win_mean(data, fast), oa_core::win_mean(data, slow))
    };
    one(zip(&f, &s, |x, y| x - y))
}

fn medprice(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    one(zip(high, low, |h, l| (h + l) / 2.0))
}

fn typprice(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    one((0..close.len())
        .map(|i| (high[i] + low[i] + close[i]) / 3.0)
        .collect())
}

fn wclprice(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    one((0..close.len())
        .map(|i| (high[i] + low[i] + 2.0 * close[i]) / 4.0)
        .collect())
}

fn avgprice(a: &Args) -> TaResult {
    let [open, high, low, close] = a.aligned::<4>()?;
    one((0..close.len())
        .map(|i| (open[i] + high[i] + low[i] + close[i]) / 4.0)
        .collect())
}

/// MIDPRICE, PLUS_DM, MINUS_DM: high, low and a period.
fn hl_period(a: &Args, kernel: fn(&[f64], &[f64], usize) -> Vec<f64>) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    one(kernel(high, low, us(a.p().int("period")?)?))
}

/// DX, ADXR: high, low, close and a period.
fn hlc_period(a: &Args, kernel: fn(&[f64], &[f64], &[f64], usize) -> Vec<f64>) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    one(kernel(high, low, close, us(a.p().int("period")?)?))
}

fn stochf(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let p = a.p();
    let (k, d) = oa_core::stochf(
        high,
        low,
        close,
        us(p.int("fastk_period")?)?,
        us(p.int("fastd_period")?)?,
    );
    many(vec![k, d])
}
