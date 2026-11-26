//! The `openalgo.ta` indicators the MCP research tools use, as pure
//! functions over `f64` slices (`NaN` marks a value that does not exist yet).
//!
//! Each kernel follows the NumPy reference in the web's SDK,
//! `openalgo/indicators/_backend.py` (the documented equivalent of its
//! compiled core), and each wrapper check follows the indicator classes
//! (`base.py:validate_period`, `trend.py`, `momentum.py`, `volatility.py`,
//! `hybrid.py`, `statistics.py`, `utils.py`). Line numbers refer to
//! `openalgo` 2.0.5 as installed with the web.
//!
//! Errors carry the Python exception type and message the web would report,
//! since the tools put them in their output.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `kernels` | the `_backend.py` kernels (`sma_k`, `rsi_k`, `supertrend_k`, ...) |
//! | `surface` | `ta.<name>(...)`: argument binding, wrapper checks, results |
//! | here | the Python errors and the shared period and input checks |

mod kernels;
mod surface;

pub use kernels::*;
pub use surface::*;

/// A Python exception: type name and message.
#[derive(Debug, Clone, PartialEq)]
pub struct PyErr {
    pub kind: &'static str,
    pub message: String,
}

impl PyErr {
    pub fn value(msg: impl Into<String>) -> Self {
        Self {
            kind: "ValueError",
            message: msg.into(),
        }
    }
    pub fn type_err(msg: impl Into<String>) -> Self {
        Self {
            kind: "TypeError",
            message: msg.into(),
        }
    }
}

pub type R<T> = Result<T, PyErr>;

const NAN: f64 = f64::NAN;

/// `base.py:112` `validate_period`.
pub fn validate_period(period: i64, len: usize) -> R<()> {
    if period <= 0 {
        return Err(PyErr::value(format!(
            "Period must be positive, got {}",
            period
        )));
    }
    if period as usize > len {
        return Err(PyErr::value(format!(
            "Period ({}) cannot be greater than data length ({})",
            period, len
        )));
    }
    Ok(())
}

/// `base.py:24` `validate_input`: empty input is refused.
fn non_empty(d: &[f64]) -> R<()> {
    if d.is_empty() {
        Err(PyErr::value("Input data cannot be empty"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Reference values derived by hand from the formulas in
    //! `openalgo/indicators/_backend.py` (line numbers on each test).
    use super::*;
    use serde_json::{json, Map, Value};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 || (a.is_nan() && b.is_nan())
    }

    fn same(got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{:?}", got);
        for (g, w) in got.iter().zip(want) {
            assert!(close(*g, *w), "got {:?} want {:?}", got, want);
        }
    }

    #[test]
    fn sma_window_mean() {
        // _backend.py:75: mean of each full window; NaN before period-1.
        same(
            &sma_k(&[1.0, 2.0, 3.0, 4.0, 5.0], 3),
            &[NAN, NAN, 2.0, 3.0, 4.0],
        );
        // Window-local NaN: only the windows holding the NaN are blank.
        same(
            &sma_k(&[1.0, NAN, 3.0, 4.0, 5.0], 2),
            &[NAN, NAN, NAN, 3.5, 4.5],
        );
    }

    #[test]
    fn ema_seeds_at_first_value() {
        // _backend.py:128: alpha = 2/(2+1); e0 = 1, e1 = 2/3*2 + 1/3*1 = 5/3,
        // e2 = 2/3*3 + 1/3*5/3 = 23/9.
        same(&ema_k(&[1.0, 2.0, 3.0], 2), &[1.0, 5.0 / 3.0, 23.0 / 9.0]);
    }

    #[test]
    fn wma_linear_weights() {
        // _backend.py:89: weights 1,2,3 over sum 6: (1+4+9)/6, (2+6+12)/6.
        same(
            &wma_k(&[1.0, 2.0, 3.0, 4.0], 3),
            &[NAN, NAN, 14.0 / 6.0, 20.0 / 6.0],
        );
    }

    #[test]
    fn rsi_wilder() {
        // _backend.py:201, period 2 on [1,2,3,2]: deltas 1,1,-1. Seed
        // gains mean(1,1)=1, losses 0 -> 100 at index 2. Then gain
        // (1*1+0)/2=0.5, loss (0+1)/2=0.5 -> RS 1 -> 50 at index 3.
        same(&rsi_k(&[1.0, 2.0, 3.0, 2.0], 2), &[NAN, NAN, 100.0, 50.0]);
    }

    #[test]
    fn true_range_and_atr() {
        // _backend.py:164: tr0 = h-l; tri = max(h-l, |h-c1|, |l-c1|).
        let (h, l, c) = ([10.0, 11.0, 13.0], [8.0, 9.0, 10.0], [9.0, 10.0, 12.0]);
        // tr = [2, max(2,2,0)=2, max(3,3,0)=3]
        same(&true_range_k(&h, &l, &c), &[2.0, 2.0, 3.0]);
        // _backend.py:186/406: seed mean(2,2)=2 at 1, then (2*1+3)/2=2.5.
        same(&atr_k(&h, &l, &c, 2), &[NAN, 2.0, 2.5]);
    }

    #[test]
    fn bollinger_population_stdev() {
        // _backend.py:241/148: mean 2, var (1+4+9)/3 - 4 = 2/3.
        let sd = (2.0f64 / 3.0).sqrt();
        let out = call(
            "bbands",
            &[&[1.0, 2.0, 3.0]],
            json!({"period": 3}).as_object().unwrap(),
        )
        .unwrap();
        match out {
            Out::Many(v) => {
                same(&v[0], &[NAN, NAN, 2.0 + 2.0 * sd]);
                same(&v[1], &[NAN, NAN, 2.0]);
                same(&v[2], &[NAN, NAN, 2.0 - 2.0 * sd]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn macd_is_ema_difference() {
        // _backend.py:234: line = ema(fast) - ema(slow); signal = ema(line).
        let d = [1.0, 2.0, 3.0];
        let out = call(
            "macd",
            &[&d],
            json!({"fast_period": 1, "slow_period": 2, "signal_period": 2})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        // ema(1) = data; ema(2) = [1, 5/3, 23/9]; line = [0, 1/3, 4/9].
        // signal: [0, 2/3*1/3 + 0 = 2/9, 2/3*4/9 + 1/3*2/9 = 10/27].
        match out {
            Out::Many(v) => {
                same(&v[0], &[0.0, 1.0 / 3.0, 4.0 / 9.0]);
                same(&v[1], &[0.0, 2.0 / 9.0, 10.0 / 27.0]);
                same(&v[2], &[0.0, 1.0 / 9.0, 4.0 / 9.0 - 10.0 / 27.0]);
            }
            _ => panic!(),
        }
        let e = call(
            "macd",
            &[&d],
            json!({"fast_period": 3, "slow_period": 2})
                .as_object()
                .unwrap(),
        )
        .unwrap_err();
        assert_eq!(e.message, "Fast period must be less than slow period");
    }

    #[test]
    fn supertrend_flips_up_on_close_above_band() {
        // _backend.py:2036 with period 1, multiplier 1: atr = tr.
        // bar0: tr 2, hl2 9 -> ub 11, lb 7, dir +1, st 11.
        // bar1: h 14 l 12 c 13.5: tr max(2, |14-9|=5, |12-9|=3)=5 -> hl2 13,
        //   ub 18, lb 8. fl: 8 > 7 -> 8. fu: 18 < 11? no; c0 9 > 11? no -> 11.
        //   st0 == fu0 so dir = close 13.5 > 11 -> -1 (up); st = fl = 8.
        let (st, dr) = supertrend_k(&[10.0, 14.0], &[8.0, 12.0], &[9.0, 13.5], 1, 1.0);
        same(&st, &[11.0, 8.0]);
        same(&dr, &[1.0, -1.0]);
    }

    #[test]
    fn stochastic_and_williams() {
        // _backend.py:249 k=2, smooth 1, d 1: fast k at 1 = 100*(c-ll)/(hh-ll).
        let (h, l, c) = ([10.0, 12.0], [8.0, 9.0], [9.0, 11.0]);
        let (k, d) = stochastic_k(&h, &l, &c, 2, 1, 1);
        // hh 12, ll 8 -> 100*(11-8)/4 = 75.
        same(&k, &[NAN, 75.0]);
        same(&d, &[NAN, 75.0]);
        // _backend.py:299: -100*(12-11)/4 = -25.
        same(&williams_r_k(&h, &l, &c, 2), &[NAN, -25.0]);
    }

    #[test]
    fn cci_mean_deviation() {
        // _backend.py:267, period 2: tp = [3, 6]; mean 4.5; md 1.5;
        // (6 - 4.5) / (0.015 * 1.5) = 66.666...
        same(
            &cci_k(&[4.0, 7.0], &[2.0, 5.0], &[3.0, 6.0], 2),
            &[NAN, 1.5 / 0.0225],
        );
    }

    #[test]
    fn pivot_points_classic() {
        // _backend.py:781: h 12 l 8 c 10 -> p 10, r1 12, s1 8, r2 14, s2 6, r3 16, s3 4.
        let p = pivot_points_k(&[12.0], &[8.0], &[10.0]);
        let flat: Vec<f64> = p.iter().map(|s| s[0]).collect();
        same(&flat, &[10.0, 12.0, 8.0, 14.0, 6.0, 16.0, 4.0]);
    }

    #[test]
    fn regression_correlation_beta() {
        // _backend.py:919: on a straight line the fitted end value moves by
        // the slope each bar -> 2.
        same(
            &lrslope_k(&[1.0, 3.0, 5.0, 7.0], 2, 1.0),
            &[NAN, NAN, 2.0, 2.0],
        );
        // _backend.py:950: y = 2x is perfectly correlated.
        same(
            &correl_k(&[1.0, 2.0, 4.0], &[2.0, 4.0, 8.0], 3),
            &[NAN, NAN, 1.0],
        );
        // _backend.py:967: asset changes are twice the market's -> beta 2.
        same(
            &beta_k(&[1.0, 3.0, 4.0], &[1.0, 2.0, 2.5], 2),
            &[NAN, NAN, 2.0],
        );
    }

    #[test]
    fn adx_on_steady_uptrend() {
        // _backend.py:728: every bar moves up by 1 with range 2, so -DM is 0,
        // +DI = 100 * mean(+DM) / ATR and DX = 100 once both are seeded.
        let h = [10.0, 11.0, 12.0, 13.0];
        let l = [8.0, 9.0, 10.0, 11.0];
        let c = [9.0, 10.0, 11.0, 12.0];
        let (p, m, a) = adx_k(&h, &l, &c, 2);
        // tr = [2, 2, 2, 2]; +dm = [0, 1, 1, 1]; rma(2): tr [_, 2, 2, 2],
        // +dm [_, 0.5, 0.75, 0.875] -> +DI [_, 25, 37.5, 43.75]; -DI 0;
        // dx 100 from index 1 -> adx seeded mean(100,100) at index 2.
        same(&p, &[NAN, 25.0, 37.5, 43.75]);
        same(&m, &[NAN, 0.0, 0.0, 0.0]);
        same(&a, &[NAN, NAN, 100.0, 100.0]);
    }

    #[test]
    fn hv_annualised_log_returns() {
        // _backend.py:1884, length 2: returns ln2, ln2 -> stdev 0.
        same(&hv_k(&[1.0, 2.0, 4.0], 2, 365.0, 1.0), &[NAN, NAN, 0.0]);
    }

    #[test]
    fn ichimoku_shifts() {
        // _backend.py:2101, conv 1, base 1, span2 1, disp 2: lines are hl2,
        // leading spans shift forward 1, lagging span shifts back 1.
        let v = ichimoku_k(
            &[2.0, 4.0, 6.0],
            &[0.0, 2.0, 4.0],
            &[1.0, 3.0, 5.0],
            1,
            1,
            1,
            2,
        );
        same(&v[0], &[1.0, 3.0, 5.0]);
        same(&v[2], &[NAN, 1.0, 3.0]);
        same(&v[4], &[3.0, 5.0, NAN]);
    }

    #[test]
    fn crossings() {
        // utils.py:25/54.
        assert_eq!(
            crossover(&[1.0, 3.0, 2.0], &[2.0, 2.0, 2.0]),
            vec![false, true, false]
        );
        assert_eq!(
            crossunder(&[3.0, 1.0, 1.0], &[2.0, 2.0, 2.0]),
            vec![false, true, false]
        );
    }

    #[test]
    fn wrapper_errors_match_python() {
        let m = Map::new();
        let e = call("ema", &[&[1.0, 2.0]], &m).unwrap_err();
        assert_eq!(e.kind, "TypeError");
        assert_eq!(
            e.message,
            "ema() missing 1 required positional argument: 'period'"
        );
        let e = call("rsi", &[&[1.0, 2.0]], &m).unwrap_err();
        assert_eq!(
            e.message,
            "Period (14) cannot be greater than data length (2)"
        );
        let e = call(
            "rsi",
            &[&[1.0, 2.0]],
            json!({"length": 3}).as_object().unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            e.message,
            "rsi() got an unexpected keyword argument 'length'"
        );
        let e = call("atr", &[&[1.0]], &m).unwrap_err();
        assert_eq!(
            e.message,
            "atr() missing 2 required positional arguments: 'low' and 'close'"
        );
    }

    #[test]
    fn python_rounding() {
        assert_eq!(py_round(1.23456, 4), 1.2346);
        assert_eq!(last(&[1.0, 2.123456, NAN]), Some(2.1235));
        assert_eq!(last(&[NAN]), None);
    }

    /// Last values from the web's own `openalgo.ta` (2.0.5, compiled core)
    /// on the same 80-bar series, produced once with the web's Python
    /// environment. Every supported function must agree to 1e-9.
    #[test]
    fn agrees_with_the_web_library_on_a_long_series() {
        let n = 80;
        let close: Vec<f64> = (0..n)
            .map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.1)
            .collect();
        let high: Vec<f64> = (0..n)
            .map(|i| close[i] + 1.0 + (i as f64).cos().abs())
            .collect();
        let low: Vec<f64> = (0..n)
            .map(|i| close[i] - 1.0 - (i as f64 * 1.3).sin().abs())
            .collect();
        let mkt: Vec<f64> = (0..n)
            .map(|i| 50.0 + (i as f64 * 0.5).cos() * 3.0 + i as f64 * 0.05)
            .collect();
        let reference: Value = serde_json::from_str(
            r#"{"sma": 106.47148224510313, "ema": 106.42827576863061, "wma": 106.76969234365465, "rsi": 44.31344217891444, "macd": [0.16165225409203288, 0.6791402514451601, -0.5174879973531272], "supertrend": [99.93802401689173, -1.0], "adx": [22.400901919440468, 28.91445081977545, 11.29172308766731], "ichimoku": [107.4496400262411, 106.59300648520994, 104.66826703626509, 102.95856145274135, 103.15718153140847], "stochastic": [27.00251958205408, 43.10122857003811], "cci": -65.26987676503794, "williams_r": -78.4540803241933, "atr": 4.066581739072121, "natr": 3.942121797728606, "bbands": [113.84819896559179, 106.47148224510313, 99.09476552461447], "bbpercent": 0.27535393866434155, "bbwidth": 0.13856699587425542, "keltner": [114.55385370225757, 106.4287269781737, 98.30360025408983], "donchian": [113.56842732723733, 106.59300648520994, 99.61758564318257], "hv": 40.91355563580153, "pivot_points": [103.18039557828426, 105.02993843132367, 101.30763867836909, 106.90269533123885, 99.45809582532968, 108.75223818427826, 97.5853389254145], "lrslope": -0.9018372198264473, "correlation": 0.4578237642000194, "beta": 1.0352753615105659, "highest": 113.56842732723733, "lowest": 99.61758564318257}"#,
        )
        .unwrap();
        let p = |v: Value| v.as_object().cloned().unwrap();
        let hlc: [&[f64]; 3] = [&high, &low, &close];
        let cases: Vec<(&str, Vec<&[f64]>, Value)> = vec![
            ("sma", vec![&close], json!({"period": 20})),
            ("ema", vec![&close], json!({"period": 20})),
            ("wma", vec![&close], json!({"period": 10})),
            ("rsi", vec![&close], json!({"period": 14})),
            ("macd", vec![&close], json!({})),
            ("supertrend", hlc.to_vec(), json!({})),
            ("adx", hlc.to_vec(), json!({"period": 14})),
            ("ichimoku", hlc.to_vec(), json!({})),
            ("stochastic", hlc.to_vec(), json!({})),
            ("cci", hlc.to_vec(), json!({"period": 20})),
            ("williams_r", hlc.to_vec(), json!({"period": 14})),
            ("atr", hlc.to_vec(), json!({"period": 14})),
            ("natr", hlc.to_vec(), json!({"period": 14})),
            (
                "bbands",
                vec![&close],
                json!({"period": 20, "std_dev": 2.0}),
            ),
            (
                "bbpercent",
                vec![&close],
                json!({"period": 20, "std_dev": 2.0}),
            ),
            (
                "bbwidth",
                vec![&close],
                json!({"period": 20, "std_dev": 2.0}),
            ),
            ("keltner", hlc.to_vec(), json!({})),
            ("donchian", vec![&high, &low], json!({"period": 20})),
            ("hv", vec![&close], json!({})),
            ("pivot_points", hlc.to_vec(), json!({})),
            ("lrslope", vec![&close], json!({"period": 20})),
            ("correlation", vec![&close, &mkt], json!({"period": 20})),
            ("beta", vec![&close, &mkt], json!({"period": 20})),
            ("highest", vec![&high], json!({"period": 20})),
            ("lowest", vec![&low], json!({"period": 20})),
        ];
        assert_eq!(
            cases.len(),
            supported().len() - 2,
            "stdev and true_range are covered above"
        );
        let lastv = |s: &[f64]| s.iter().rev().find(|x| !x.is_nan()).copied().unwrap();
        for (name, series, params) in cases {
            let got: Vec<f64> = match call(name, &series, &p(params)).unwrap() {
                Out::One(v) => vec![lastv(&v)],
                Out::Many(v) => v.iter().map(|s| lastv(s)).collect(),
            };
            let want: Vec<f64> = match &reference[name] {
                Value::Array(a) => a.iter().map(|x| x.as_f64().unwrap()).collect(),
                x => vec![x.as_f64().unwrap()],
            };
            assert_eq!(got.len(), want.len(), "{}", name);
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    (g - w).abs() < 1e-9,
                    "{}: got {:?} want {:?}",
                    name,
                    got,
                    want
                );
            }
        }
    }

    #[test]
    fn web_list_covers_supported() {
        for n in supported() {
            assert!(WEB_TA_FUNCTIONS.contains(&n), "{}", n);
        }
        assert_eq!(WEB_TA_FUNCTIONS.len(), 128);
    }
}
