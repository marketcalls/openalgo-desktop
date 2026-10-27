//! The 13 helpers on `ta` (`crossover`, `highest`, `exrem`, ...). Except
//! `roc` and `stdev`, which go to the compiled kernels, they are the
//! interpreted loops in `openalgo/indicators/utils.py`, ported as such.
//! Signal arrays arrive as floats, and a bar counts as a signal where Python
//! reads the float as true: any value but 0, NaN included.

use std::collections::VecDeque;

use super::numpy::nan_vec;
use super::{one, us, Args, Series, TaResult};

pub(super) fn call(name: &str, a: &Args) -> Option<TaResult> {
    Some(match name {
        "crossover" => pair(a, |p0, q0, p1, q1| p1 > q1 && p0 <= q0),
        "crossunder" => pair(a, |p0, q0, p1, q1| p1 < q1 && p0 >= q0),
        "cross" => pair(a, |p0, q0, p1, q1| {
            (p1 > q1 && p0 <= q0) || (p1 < q1 && p0 >= q0)
        }),
        "highest" => extreme(a, |kept, x| kept <= x),
        "lowest" => extreme(a, |kept, x| kept >= x),
        "change" => change(a),
        "roc" => kernel(a, "length", oa_core::roc),
        "stdev" => kernel(a, "period", oa_core::stdev),
        "exrem" => exrem(a),
        "flip" => flip(a),
        "valuewhen" => valuewhen(a),
        "rising" => compare_back(a, |now, then| now > then),
        "falling" => compare_back(a, |now, then| now < then),
        _ => return None,
    })
}

/// `crossover`, `crossunder`, `cross`: false on the first bar and wherever
/// either bar of either series is NaN.
fn pair(a: &Args, hit: fn(f64, f64, f64, f64) -> bool) -> TaResult {
    let (first, second) = (a.s(0)?, a.s(1)?);
    let n = first.len().min(second.len());
    let mut out = vec![false; first.len()];
    for i in 1..n {
        let values = [first[i - 1], second[i - 1], first[i], second[i]];
        if values.iter().all(|x| !x.is_nan()) {
            out[i] = hit(values[0], values[1], values[2], values[3]);
        }
    }
    Ok(vec![Series::Bool(out)])
}

/// `highest`, `lowest`: the monotonic-queue window extreme. `drop_back`
/// says when the newest value displaces the one at the back of the queue.
fn extreme(a: &Args, drop_back: fn(f64, f64) -> bool) -> TaResult {
    let data = a.s(0)?;
    let period = a.p().int("period")?;
    let mut out = nan_vec(data.len());
    let mut queue: VecDeque<(f64, i64)> = VecDeque::new();
    for (i, &x) in data.iter().enumerate() {
        let i = i as i64;
        while queue.front().is_some_and(|&(_, at)| at <= i - period) {
            queue.pop_front();
        }
        while queue.back().is_some_and(|&(kept, _)| drop_back(kept, x)) {
            queue.pop_back();
        }
        queue.push_back((x, i));
        if i >= period - 1 {
            if let Some(&(front, _)) = queue.front() {
                out[i as usize] = front;
            }
        }
    }
    one(out)
}

/// `change`: `x[i] - x[i - length]`.
fn change(a: &Args) -> TaResult {
    let data = a.s(0)?;
    let length = us(a.p().int("length")?)?;
    let mut out = nan_vec(data.len());
    for i in length..data.len() {
        out[i] = data[i] - data[i - length];
    }
    one(out)
}

/// `roc` and `stdev`: straight to the kernel, with no period check.
fn kernel(a: &Args, param: &str, f: fn(&[f64], usize) -> Vec<f64>) -> TaResult {
    one(f(a.s(0)?, us(a.p().int(param)?)?))
}

fn truthy(x: f64) -> bool {
    x != 0.0
}

/// `exrem`: the first primary signal, then nothing until a secondary one.
fn exrem(a: &Args) -> TaResult {
    let (primary, secondary) = (a.s(0)?, a.s(1)?);
    let mut out = vec![false; primary.len()];
    let mut active = false;
    for i in 0..primary.len() {
        if !active && truthy(primary[i]) {
            out[i] = true;
            active = true;
        } else if secondary.get(i).copied().is_some_and(truthy) {
            active = false;
        }
    }
    Ok(vec![Series::Bool(out)])
}

/// `flip`: on from a primary signal until a secondary one.
fn flip(a: &Args) -> TaResult {
    let (primary, secondary) = (a.s(0)?, a.s(1)?);
    let mut out = vec![false; primary.len()];
    let mut active = false;
    for i in 0..primary.len() {
        if truthy(primary[i]) {
            active = true;
        } else if secondary.get(i).copied().is_some_and(truthy) {
            active = false;
        }
        out[i] = active;
    }
    Ok(vec![Series::Bool(out)])
}

/// `valuewhen`: `array` at the `n`th most recent bar where `expr` fired,
/// remembering at most the last 1000 such bars.
fn valuewhen(a: &Args) -> TaResult {
    let (expr, array) = (a.s(0)?, a.s(1)?);
    let nth = a.p().int("n")?;
    let length = expr.len();
    let max_lookback = length.min(1000);
    let mut fired: VecDeque<usize> = VecDeque::with_capacity(max_lookback);
    let mut out = nan_vec(length);
    for i in 0..length {
        if truthy(expr[i]) {
            if fired.len() >= max_lookback {
                fired.pop_front();
            }
            fired.push_back(i);
        }
        if nth >= 1 && fired.len() as i64 >= nth {
            let at = fired[fired.len() - nth as usize];
            out[i] = array.get(at).copied().unwrap_or(f64::NAN);
        }
    }
    one(out)
}

/// `rising`, `falling`: compared with `length` bars back, false where either
/// is NaN.
fn compare_back(a: &Args, op: fn(f64, f64) -> bool) -> TaResult {
    let data = a.s(0)?;
    let length = us(a.p().int("length")?)?;
    let mut out = vec![false; data.len()];
    for i in length..data.len() {
        let (now, then) = (data[i], data[i - length]);
        if !now.is_nan() && !then.is_nan() {
            out[i] = op(now, then);
        }
    }
    Ok(vec![Series::Bool(out)])
}
