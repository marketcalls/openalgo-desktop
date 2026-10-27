//! The NumPy operations the `openalgo.ta` wrappers compose kernels with,
//! reproduced to the bit where NumPy's result depends on its algorithm.
//!
//! Plain element-wise arithmetic (`a + b`, `a * 2.0`) is IEEE-identical in
//! Rust and needs nothing here. What does: `np.sum` and `np.mean` (pairwise
//! summation, not a running total), `np.minimum` and `np.clip` (a NaN wins,
//! where `f64::min` would drop it), `np.sort` (NaN sorts last) and the
//! `_backend.py` helpers `_roll` and `_shift`.

/// `np.full(n, np.nan)`.
pub(super) fn nan_vec(n: usize) -> Vec<f64> {
    vec![f64::NAN; n]
}

/// `np.sum` / `np.add.reduce` over a contiguous float64 array: NumPy's
/// pairwise summation (`loops_utils.h`), eight accumulators per block of up to
/// 128 values, halves above that. A running total differs in the last bits.
pub(super) fn sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut res = 0.0;
        for &x in a {
            res += x;
        }
        return res;
    }
    if n <= 128 {
        let mut r = [0.0; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for (j, acc) in r.iter_mut().enumerate() {
                *acc += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        return res;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    sum(&a[..n2]) + sum(&a[n2..])
}

/// `np.mean`: the pairwise sum divided by the count.
pub(super) fn mean(a: &[f64]) -> f64 {
    sum(a) / a.len() as f64
}

/// `np.nansum`: NaN counted as zero, then `np.sum`.
pub(super) fn nansum(a: &[f64]) -> f64 {
    let clean: Vec<f64> = a
        .iter()
        .map(|&x| if x.is_nan() { 0.0 } else { x })
        .collect();
    sum(&clean)
}

/// `_backend._roll(data, period, fn)`: `fn` over each full window, NaN before.
pub(super) fn roll(data: &[f64], period: usize, f: fn(&[f64]) -> f64) -> Vec<f64> {
    let n = data.len();
    let mut out = nan_vec(n);
    if period == 0 {
        return out;
    }
    for i in (period - 1)..n {
        out[i] = f(&data[i + 1 - period..=i]);
    }
    out
}

/// `_backend._shift(arr, k)`: values moved `k` bars later, NaN in front; a
/// shift outside `0..len` leaves every bar NaN.
pub(super) fn shift(arr: &[f64], k: i64) -> Vec<f64> {
    let n = arr.len();
    let mut out = nan_vec(n);
    if k >= 0 && (k as usize) < n {
        let k = k as usize;
        out[k..].copy_from_slice(&arr[..n - k]);
    }
    out
}

/// `np.minimum` of two values: a NaN on either side is the answer.
pub(super) fn minimum(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// `np.clip(x, lo, hi)`: a NaN stays NaN.
pub(super) fn clip(x: f64, lo: f64, hi: f64) -> f64 {
    if x.is_nan() {
        x
    } else {
        let raised = if x < lo { lo } else { x };
        if raised > hi {
            hi
        } else {
            raised
        }
    }
}

/// `np.cumsum`: a running total, NaN carried forward once it appears.
pub(super) fn cumsum(a: &[f64]) -> Vec<f64> {
    let mut total = 0.0;
    a.iter()
        .map(|&x| {
            total += x;
            total
        })
        .collect()
}

/// `np.sort` of one window, NaN last as NumPy orders it.
pub(super) fn sorted(window: &[f64]) -> Vec<f64> {
    let mut w = window.to_vec();
    w.sort_by(|a, b| match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
    });
    w
}

/// Element-wise `a op b` over two equal-length series.
pub(super) fn zip(a: &[f64], b: &[f64], op: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    a.iter().zip(b).map(|(&x, &y)| op(x, y)).collect()
}

/// Element-wise `op(x)` over one series.
pub(super) fn map(a: &[f64], op: impl Fn(f64) -> f64) -> Vec<f64> {
    a.iter().map(|&x| op(x)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairwise_sum_matches_numpy_blocks() {
        // Values NumPy 2.5 returns for these inputs (np.sum), which a running
        // total misses in the last bit.
        let a: Vec<f64> = (0..37).map(|i| 0.1 * i as f64 + 1e-3).collect();
        let running: f64 = a.iter().sum();
        assert_eq!(running, 66.63700000000001);
        assert_eq!(sum(&a), 66.637);
        let b = [1e16, 1.0, -1e16, 1.0, 3.0, 1e-3, 7.0, 1e16, 5.0, -1e16, 2.0];
        assert_eq!(sum(&b), 18.0);
        assert_eq!(sum(&[]), 0.0);
        assert_eq!(sum(&[1.5, 2.5]), 4.0);
        assert!(sum(&[1.0, f64::NAN]).is_nan());
    }

    #[test]
    fn minimum_and_clip_keep_nan() {
        assert!(minimum(1.0, f64::NAN).is_nan());
        assert!(minimum(f64::NAN, 1.0).is_nan());
        assert_eq!(minimum(1.0, 2.0), 1.0);
        assert!(clip(f64::NAN, 0.0, 100.0).is_nan());
        assert_eq!(clip(-3.0, 0.0, 100.0), 0.0);
        assert_eq!(clip(130.0, 0.0, 100.0), 100.0);
    }

    #[test]
    fn shift_outside_the_series_is_all_nan() {
        assert!(shift(&[1.0, 2.0], 2).iter().all(|x| x.is_nan()));
        assert!(shift(&[1.0, 2.0], -1).iter().all(|x| x.is_nan()));
        let moved = shift(&[1.0, 2.0, 3.0], 1);
        assert!(moved[0].is_nan());
        assert_eq!(&moved[1..], &[1.0, 2.0]);
    }

    #[test]
    fn sort_puts_nan_last() {
        let s = sorted(&[3.0, f64::NAN, 1.0]);
        assert_eq!(&s[..2], &[1.0, 3.0]);
        assert!(s[2].is_nan());
    }
}
