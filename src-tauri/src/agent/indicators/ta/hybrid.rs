//! `openalgo/indicators/hybrid.py` (the 7 hybrid methods) and the
//! `_backend.py` compositions they call.

use super::{many, one, us, validate_period, Args, Series, TaError, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "adx" => adx(a),
        "dmi" => adx(a).map(|mut lines| {
            lines.truncate(2);
            lines
        }),
        "aroon" => aroon(a),
        "pivot_points" => pivot_points(a),
        "psar" => psar(a),
        "fractals" => fractals(a),
        "rwi" => rwi(a),
        _ => return None,
    })
}

/// `ADX.calculate`: (plus DI, minus DI, ADX). `DMI.calculate` is its first
/// two lines.
fn adx(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    let (plus, minus, adx) = oa_core::adx(high, low, close, us(period)?);
    many(vec![plus, minus, adx])
}

/// `Aroon.calculate`.
fn aroon(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let period = a.p().int("period")?;
    validate_period(period, high.len())?;
    let (up, down) = oa_core::aroon(high, low, us(period)?);
    many(vec![up, down])
}

/// `PivotPoints.calculate` and `_backend.pivot_points`, per bar.
fn pivot_points(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let n = close.len();
    let mut lines: Vec<Vec<f64>> = (0..7).map(|_| Vec::with_capacity(n)).collect();
    for i in 0..n {
        let (h, l, c) = (high[i], low[i], close[i]);
        let pivot = (h + l + c) / 3.0;
        let levels = [
            pivot,
            2.0 * pivot - l,
            2.0 * pivot - h,
            pivot + (h - l),
            pivot - (h - l),
            h + 2.0 * (pivot - l),
            l - 2.0 * (h - pivot),
        ];
        for (line, level) in lines.iter_mut().zip(levels) {
            line.push(level);
        }
    }
    many(lines)
}

/// `ta.psar`: `SAR.calculate`'s SAR line only.
fn psar(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let p = a.p();
    let acceleration = p.float("acceleration")?;
    let maximum = p.float("maximum")?;
    if acceleration <= 0.0 || maximum <= 0.0 {
        return Err(TaError::value("Acceleration and maximum must be positive"));
    }
    if acceleration > maximum {
        return Err(TaError::value(
            "Acceleration cannot be greater than maximum",
        ));
    }
    let (sar, _) = oa_core::sar(high, low, acceleration, maximum);
    one(sar)
}

/// `WilliamsFractals.calculate`: boolean flags.
fn fractals(a: &Args) -> TaResult {
    let [high, low] = a.aligned::<2>()?;
    let periods = a.p().int("periods")?;
    if periods < 2 {
        return Err(TaError::value(format!(
            "Periods must be at least 2, got {}",
            periods
        )));
    }
    let (up, down) = oa_core::williams_fractals(high, low, us(periods)?);
    Ok(vec![Series::Bool(up), Series::Bool(down)])
}

/// `RWI.calculate`.
fn rwi(a: &Args) -> TaResult {
    let [high, low, close] = a.aligned::<3>()?;
    let period = a.p().int("period")?;
    validate_period(period, close.len())?;
    let (up, down) = oa_core::rwi(high, low, close, us(period)?);
    many(vec![up, down])
}
