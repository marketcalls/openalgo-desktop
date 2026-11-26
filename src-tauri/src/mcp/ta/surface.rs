//! The `ta.<name>(...)` surface: argument binding, the wrapper checks of
//! the indicator classes, and the result of each call.

use super::kernels::{
    adx_k, atr_k, beta_k, cci_k, correl_k, ema_k, ema_sma_k, highest_k, hv_k, ichimoku_k, lowest_k,
    lrslope_k, pivot_points_k, rsi_k, sma_k, stdev_k, stochastic_k, supertrend_k, true_range_k,
    williams_r_k, wma_k,
};
use super::{non_empty, validate_period, PyErr, R};
use serde_json::{Map, Value};

/// An indicator's result: one series or a tuple of series.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    One(Vec<f64>),
    Many(Vec<Vec<f64>>),
}

/// A scalar parameter of a `ta` function.
#[derive(Debug, Clone, Copy)]
struct Sc {
    name: &'static str,
    /// Default, or `None` when required.
    default: Option<f64>,
    int: bool,
}

const fn int(name: &'static str, d: Option<f64>) -> Sc {
    Sc {
        name,
        default: d,
        int: true,
    }
}
const fn flt(name: &'static str, d: f64) -> Sc {
    Sc {
        name,
        default: Some(d),
        int: false,
    }
}

/// Signature of one supported function: series parameters, then scalars.
struct Sig {
    name: &'static str,
    series: &'static [&'static str],
    scalars: &'static [Sc],
}

const HLC: &[&str] = &["high", "low", "close"];

/// Every `ta` function the desktop computes (names as in `openalgo.ta`).
const SIGS: &[Sig] = &[
    Sig {
        name: "sma",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "ema",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "wma",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "rsi",
        series: &["data"],
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "macd",
        series: &["data"],
        scalars: &[
            int("fast_period", Some(12.0)),
            int("slow_period", Some(26.0)),
            int("signal_period", Some(9.0)),
        ],
    },
    Sig {
        name: "supertrend",
        series: HLC,
        scalars: &[int("period", Some(10.0)), flt("multiplier", 3.0)],
    },
    Sig {
        name: "ichimoku",
        series: HLC,
        scalars: &[
            int("conversion_periods", Some(9.0)),
            int("base_periods", Some(26.0)),
            int("lagging_span2_periods", Some(52.0)),
            int("displacement", Some(26.0)),
        ],
    },
    Sig {
        name: "stochastic",
        series: HLC,
        scalars: &[
            int("k_period", Some(14.0)),
            int("smooth_k", Some(3.0)),
            int("d_period", Some(3.0)),
        ],
    },
    Sig {
        name: "cci",
        series: HLC,
        scalars: &[int("period", Some(20.0))],
    },
    Sig {
        name: "williams_r",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "atr",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "natr",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "true_range",
        series: HLC,
        scalars: &[],
    },
    Sig {
        name: "bbands",
        series: &["data"],
        scalars: &[int("period", Some(20.0)), flt("std_dev", 2.0)],
    },
    Sig {
        name: "bbpercent",
        series: &["data"],
        scalars: &[int("period", Some(20.0)), flt("std_dev", 2.0)],
    },
    Sig {
        name: "bbwidth",
        series: &["data"],
        scalars: &[int("period", Some(20.0)), flt("std_dev", 2.0)],
    },
    Sig {
        name: "keltner",
        series: HLC,
        scalars: &[
            int("ema_period", Some(20.0)),
            int("atr_period", Some(10.0)),
            flt("multiplier", 2.0),
        ],
    },
    Sig {
        name: "donchian",
        series: &["high", "low"],
        scalars: &[int("period", Some(20.0))],
    },
    Sig {
        name: "hv",
        series: &["close"],
        scalars: &[
            int("length", Some(10.0)),
            int("annual", Some(365.0)),
            int("per", Some(1.0)),
        ],
    },
    Sig {
        name: "adx",
        series: HLC,
        scalars: &[int("period", Some(14.0))],
    },
    Sig {
        name: "pivot_points",
        series: HLC,
        scalars: &[],
    },
    Sig {
        name: "highest",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "lowest",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "stdev",
        series: &["data"],
        scalars: &[int("period", None)],
    },
    Sig {
        name: "lrslope",
        series: &["data"],
        scalars: &[int("period", Some(100.0)), int("interval", Some(1.0))],
    },
    Sig {
        name: "correlation",
        series: &["data1", "data2"],
        scalars: &[int("period", Some(20.0))],
    },
    Sig {
        name: "beta",
        series: &["asset", "market"],
        scalars: &[int("period", Some(252.0))],
    },
];

/// Largest magnitude of a scalar indicator parameter.
pub const MAX_PARAM: f64 = 1_000_000.0;

/// Every public function of the web's `openalgo.ta` (2.4.1, web 348b7edf7:
/// 2.0.5's 127 and `rvi_volatility`), so a name the desktop does not compute
/// yet is told apart from one that does not exist.
pub const WEB_TA_FUNCTIONS: &[&str] = &[
    "accelerator_oscillator",
    "adl",
    "adx",
    "adxr",
    "alligator",
    "alma",
    "apo",
    "aroon",
    "aroon_oscillator",
    "atr",
    "avgprice",
    "awesome_oscillator",
    "bbands",
    "bbpercent",
    "bbwidth",
    "beta",
    "bop",
    "cci",
    "chaikin",
    "chandelier_exit",
    "change",
    "cho",
    "chop",
    "ckstop",
    "cmf",
    "cmo",
    "coppock",
    "correlation",
    "cross",
    "crossover",
    "crossunder",
    "crsi",
    "dema",
    "dmi",
    "donchian",
    "dpo",
    "dx",
    "elderray",
    "ema",
    "emv",
    "exrem",
    "falling",
    "fisher",
    "flip",
    "force_index",
    "fractals",
    "frama",
    "gator_oscillator",
    "highest",
    "hma",
    "hv",
    "ichimoku",
    "kama",
    "keltner",
    "kst",
    "kvo",
    "linreg",
    "linregangle",
    "linregintercept",
    "lowest",
    "lrslope",
    "ma_envelopes",
    "macd",
    "massindex",
    "mcginley",
    "median",
    "median_bands",
    "medprice",
    "mfi",
    "midpoint",
    "midprice",
    "minus_dm",
    "mode",
    "mom",
    "natr",
    "nvi",
    "nvi_with_ema",
    "obv",
    "obv_smoothed",
    "pivot_points",
    "plus_dm",
    "po",
    "ppo",
    "psar",
    "pvi",
    "pvi_with_signal",
    "pvt",
    "rising",
    "roc",
    "rocp",
    "rocr",
    "rocr100",
    "rsi",
    "rvi",
    "rvi_volatility",
    "rvol",
    "rwi",
    "sma",
    "starc",
    "stc",
    "stdev",
    "stochastic",
    "stochf",
    "stochrsi",
    "supertrend",
    "t3",
    "tema",
    "trima",
    "trix",
    "true_range",
    "tsf",
    "tsi",
    "typprice",
    "ulcerindex",
    "ultimate_oscillator",
    "uo_oscillator",
    "valuewhen",
    "variance",
    "vi",
    "vidya",
    "volosc",
    "vroc",
    "vwap",
    "vwma",
    "wclprice",
    "williams_r",
    "wma",
    "zlema",
];

/// Names the desktop computes.
pub fn supported() -> Vec<&'static str> {
    SIGS.iter().map(|s| s.name).collect()
}

pub fn is_supported(name: &str) -> bool {
    SIGS.iter().any(|s| s.name == name)
}

/// One bound argument.
enum Arg<'a> {
    Series(&'a [f64]),
    Num(f64),
}

fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 required positional argument: {}", word)
    } else {
        format!("{} required positional arguments: {}", n, word)
    }
}

fn names_list(names: &[&str]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("'{}'", n)).collect();
    match q.len() {
        0 => String::new(),
        1 => q[0].clone(),
        _ => format!("{} and {}", q[..q.len() - 1].join(", "), q[q.len() - 1]),
    }
}

/// A keyword argument's number for scalar parameter `sc`, as Python would
/// take it: an integer where the parameter is one, within MAX_PARAM.
fn scalar_value(k: &str, v: &Value, sc: &Sc) -> R<f64> {
    let num = match v {
        Value::Number(n) if sc.int && !(n.is_i64() || n.is_u64()) => {
            return Err(PyErr::type_err(format!(
                "Period must be an integer, got <class '{}'>",
                py_type(v)
            )))
        }
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(b) if !sc.int => f64::from(u8::from(*b)),
        _ => {
            return Err(PyErr::type_err(format!(
                "'{}' must be a number, got <class '{}'>",
                k,
                py_type(v)
            )))
        }
    };
    // No window, multiplier or shift needs more; larger values would
    // only overflow the index arithmetic.
    if !num.is_finite() || num.abs() > MAX_PARAM {
        return Err(PyErr::value(format!(
            "'{}' must be a number between -{} and {}",
            k, MAX_PARAM, MAX_PARAM
        )));
    }
    Ok(num)
}

/// `ta.<name>(*series, **params)` as the web's tools call it.
pub fn call(name: &str, series: &[&[f64]], params: &Map<String, Value>) -> R<Out> {
    let sig = SIGS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| PyErr::value(format!("unknown indicator '{}'", name)))?;
    let all: Vec<&str> = sig
        .series
        .iter()
        .copied()
        .chain(sig.scalars.iter().map(|s| s.name))
        .collect();
    if series.len() > all.len() {
        return Err(PyErr::type_err(format!(
            "{}() takes from {} to {} positional arguments but {} were given",
            name,
            sig.series.len() + 1,
            all.len() + 1,
            series.len() + 1
        )));
    }
    let mut bound: Vec<Option<Arg>> = (0..all.len()).map(|_| None).collect();
    for (i, s) in series.iter().enumerate() {
        bound[i] = Some(Arg::Series(s));
    }
    for (k, v) in params {
        let Some(pos) = all.iter().position(|n| n == k) else {
            return Err(PyErr::type_err(format!(
                "{}() got an unexpected keyword argument '{}'",
                name, k
            )));
        };
        if bound[pos].is_some() {
            return Err(PyErr::type_err(format!(
                "{}() got multiple values for argument '{}'",
                name, k
            )));
        }
        if pos < sig.series.len() {
            return Err(PyErr::type_err(format!(
                "Invalid input type: <class '{}'>. Expected np.ndarray, pd.Series, or list",
                py_type(v)
            )));
        }
        let sc = &sig.scalars[pos - sig.series.len()];
        bound[pos] = Some(Arg::Num(scalar_value(k, v, sc)?));
    }
    let missing: Vec<&str> = all
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            bound[*i].is_none()
                && (*i < sig.series.len() || sig.scalars[*i - sig.series.len()].default.is_none())
        })
        .map(|(_, n)| *n)
        .collect();
    if !missing.is_empty() {
        return Err(PyErr::type_err(format!(
            "{}() missing {}",
            name,
            plural(missing.len(), &names_list(&missing))
        )));
    }
    let mut ser: Vec<&[f64]> = Vec::new();
    for b in bound.iter().take(sig.series.len()) {
        match b {
            Some(Arg::Series(s)) => ser.push(s),
            _ => return Err(PyErr::type_err("Invalid input type")),
        }
    }
    let mut sc: Vec<f64> = Vec::new();
    for (j, s) in sig.scalars.iter().enumerate() {
        match &bound[sig.series.len() + j] {
            Some(Arg::Num(x)) => sc.push(*x),
            Some(Arg::Series(_)) => {
                return Err(PyErr::type_err(
                    "Period must be an integer, got <class 'pandas.core.series.Series'>",
                ))
            }
            None => sc.push(s.default.unwrap_or(f64::NAN)),
        }
    }
    for s in &ser {
        non_empty(s)?;
    }
    let len = ser[0].len();
    if ser.iter().any(|s| s.len() != len) {
        return Err(PyErr::value("All input arrays must have the same length"));
    }
    compute(name, &ser, &sc, len)
}

fn period(x: f64) -> i64 {
    x as i64
}

fn compute(name: &str, s: &[&[f64]], sc: &[f64], len: usize) -> R<Out> {
    if let Some(out) = compute_averages(name, s, sc, len)? {
        return Ok(out);
    }
    if let Some(out) = compute_oscillators(name, s, sc, len)? {
        return Ok(out);
    }
    if let Some(out) = compute_bands(name, s, sc, len)? {
        return Ok(out);
    }
    if let Some(out) = compute_levels(name, s, sc, len)? {
        return Ok(out);
    }
    Err(PyErr::value(format!("unknown indicator '{}'", name)))
}

/// Moving averages, RSI, MACD, Supertrend and Ichimoku. `None` for an indicator of another group.
fn compute_averages(name: &str, s: &[&[f64]], sc: &[f64], len: usize) -> R<Option<Out>> {
    let p0 = period(sc.first().copied().unwrap_or(0.0));
    let up = |p: i64| p as usize;
    Ok(Some(match name {
        "sma" => {
            validate_period(p0, len)?;
            Out::One(sma_k(s[0], up(p0)))
        }
        "ema" => {
            validate_period(p0, len)?;
            Out::One(ema_k(s[0], up(p0)))
        }
        "wma" => {
            validate_period(p0, len)?;
            Out::One(wma_k(s[0], up(p0)))
        }
        "stdev" => {
            validate_period(p0, len)?;
            Out::One(stdev_k(s[0], up(p0)))
        }
        "rsi" => {
            validate_period(p0, len)?;
            Out::One(rsi_k(s[0], up(p0)))
        }
        "macd" => {
            let (f, sl, sg) = (period(sc[0]), period(sc[1]), period(sc[2]));
            if f <= 0 || sl <= 0 || sg <= 0 {
                return Err(PyErr::value("All periods must be positive"));
            }
            if f >= sl {
                return Err(PyErr::value("Fast period must be less than slow period"));
            }
            let line: Vec<f64> = ema_k(s[0], up(f))
                .iter()
                .zip(ema_k(s[0], up(sl)))
                .map(|(a, b)| a - b)
                .collect();
            let signal = ema_k(&line, up(sg));
            let hist = line.iter().zip(&signal).map(|(a, b)| a - b).collect();
            Out::Many(vec![line, signal, hist])
        }
        "supertrend" => {
            validate_period(p0, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Multiplier must be positive, got {}",
                    sc[1]
                )));
            }
            let (a, b) = supertrend_k(s[0], s[1], s[2], up(p0), sc[1]);
            Out::Many(vec![a, b])
        }
        "ichimoku" => {
            for (v, n) in [
                (sc[0], "conversion_periods"),
                (sc[1], "base_periods"),
                (sc[2], "lagging_span2_periods"),
            ] {
                if v <= 0.0 {
                    return Err(PyErr::value(format!(
                        "{} must be positive, got {}",
                        n,
                        period(v)
                    )));
                }
            }
            Out::Many(ichimoku_k(
                s[0],
                s[1],
                s[2],
                up(period(sc[0])),
                up(period(sc[1])),
                up(period(sc[2])),
                period(sc[3]),
            ))
        }
        _ => return Ok(None),
    }))
}

/// Stochastic, CCI, Williams %R, ATR, NATR and true range. `None` for an
/// indicator of another group.
fn compute_oscillators(name: &str, s: &[&[f64]], sc: &[f64], len: usize) -> R<Option<Out>> {
    let p0 = period(sc.first().copied().unwrap_or(0.0));
    let up = |p: i64| p as usize;
    Ok(Some(match name {
        "stochastic" => {
            validate_period(p0, len)?;
            if sc[2] <= 0.0 {
                return Err(PyErr::value(format!(
                    "d_period must be positive, got {}",
                    period(sc[2])
                )));
            }
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "smooth_k must be positive, got {}",
                    period(sc[1])
                )));
            }
            let (k, d) = stochastic_k(
                s[0],
                s[1],
                s[2],
                up(p0),
                up(period(sc[1])),
                up(period(sc[2])),
            );
            Out::Many(vec![k, d])
        }
        "cci" => {
            validate_period(p0, len)?;
            Out::One(cci_k(s[0], s[1], s[2], up(p0)))
        }
        "williams_r" => {
            validate_period(p0, len)?;
            Out::One(williams_r_k(s[0], s[1], s[2], up(p0)))
        }
        "atr" => {
            validate_period(p0, len)?;
            Out::One(atr_k(s[0], s[1], s[2], up(p0)))
        }
        "natr" => {
            validate_period(p0, len)?;
            let atr = atr_k(s[0], s[1], s[2], up(p0));
            Out::One(
                atr.iter()
                    .zip(s[2].iter())
                    .map(|(a, c)| if *c != 0.0 { a / c * 100.0 } else { 0.0 })
                    .collect(),
            )
        }
        "true_range" => Out::One(true_range_k(s[0], s[1], s[2])),
        _ => return Ok(None),
    }))
}

/// Bollinger, Keltner and Donchian channels, and historical volatility. `None`
/// for an indicator of another group.
fn compute_bands(name: &str, s: &[&[f64]], sc: &[f64], len: usize) -> R<Option<Out>> {
    let p0 = period(sc.first().copied().unwrap_or(0.0));
    let up = |p: i64| p as usize;
    Ok(Some(match name {
        "bbands" => {
            validate_period(p0, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Standard deviation multiplier must be positive, got {}",
                    sc[1]
                )));
            }
            let (u, m, l) = bands(s[0], up(p0), sc[1]);
            Out::Many(vec![u, m, l])
        }
        "bbpercent" => {
            validate_period(p0, len)?;
            let (u, _m, l) = bands(s[0], up(p0), sc[1]);
            Out::One(
                (0..len)
                    .map(|i| {
                        if !u[i].is_nan() && u[i] == l[i] {
                            0.5
                        } else {
                            (s[0][i] - l[i]) / (u[i] - l[i])
                        }
                    })
                    .collect(),
            )
        }
        "bbwidth" => {
            validate_period(p0, len)?;
            let (u, m, l) = bands(s[0], up(p0), sc[1]);
            Out::One(
                (0..len)
                    .map(|i| {
                        if !m[i].is_nan() && m[i] == 0.0 {
                            0.0
                        } else {
                            (u[i] - l[i]) / m[i]
                        }
                    })
                    .collect(),
            )
        }
        "keltner" => {
            let (ep, ap) = (period(sc[0]), period(sc[1]));
            validate_period(ep, len)?;
            validate_period(ap, len)?;
            if sc[2] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Multiplier must be positive, got {}",
                    sc[2]
                )));
            }
            let mid = ema_sma_k(s[2], up(ep));
            let atr = atr_k(s[0], s[1], s[2], up(ap));
            let upper = (0..len).map(|i| mid[i] + sc[2] * atr[i]).collect();
            let lower = (0..len).map(|i| mid[i] - sc[2] * atr[i]).collect();
            Out::Many(vec![upper, mid, lower])
        }
        "donchian" => {
            validate_period(p0, len)?;
            let u = highest_k(s[0], up(p0));
            let l = lowest_k(s[1], up(p0));
            let m = (0..len).map(|i| (u[i] + l[i]) / 2.0).collect();
            Out::Many(vec![u, m, l])
        }
        "hv" => {
            validate_period(p0 + 1, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Annual periods must be positive, got {}",
                    period(sc[1])
                )));
            }
            if sc[2] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Per periods must be positive, got {}",
                    period(sc[2])
                )));
            }
            Out::One(hv_k(s[0], up(p0), sc[1], sc[2]))
        }
        _ => return Ok(None),
    }))
}

/// ADX, pivots, highest and lowest, regression slope, correlation and beta.
/// `None` for an indicator of another group.
fn compute_levels(name: &str, s: &[&[f64]], sc: &[f64], len: usize) -> R<Option<Out>> {
    let p0 = period(sc.first().copied().unwrap_or(0.0));
    let up = |p: i64| p as usize;
    Ok(Some(match name {
        "adx" => {
            validate_period(p0, len)?;
            let (a, b, c) = adx_k(s[0], s[1], s[2], up(p0));
            Out::Many(vec![a, b, c])
        }
        "pivot_points" => Out::Many(pivot_points_k(s[0], s[1], s[2])),
        "highest" => Out::One(highest_k(s[0], up(p0.max(0)))),
        "lowest" => Out::One(lowest_k(s[0], up(p0.max(0)))),
        "lrslope" => {
            validate_period(p0 + 1, len)?;
            if sc[1] <= 0.0 {
                return Err(PyErr::value(format!(
                    "Interval must be positive, got {}",
                    period(sc[1])
                )));
            }
            Out::One(lrslope_k(s[0], up(p0), sc[1]))
        }
        "correlation" => {
            validate_period(p0, len)?;
            Out::One(correl_k(s[0], s[1], up(p0)))
        }
        "beta" => {
            validate_period(p0 + 1, len)?;
            Out::One(beta_k(s[0], s[1], up(p0)))
        }
        _ => return Ok(None),
    }))
}

/// `_backend.py:241` `bbands`: SMA middle, population stdev bands.
fn bands(data: &[f64], period: usize, k: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let m = sma_k(data, period);
    let sd = stdev_k(data, period);
    let u = m.iter().zip(&sd).map(|(a, b)| a + k * b).collect();
    let l = m.iter().zip(&sd).map(|(a, b)| a - k * b).collect();
    (u, m, l)
}

/// Python `round(x, nd)` (correctly rounded on the binary value).
pub fn py_round(x: f64, nd: usize) -> f64 {
    format!("{:.*}", nd, x).parse().unwrap_or(x)
}

/// Web `_last`: the last non-NaN value rounded to 4 places.
pub fn last(series: &[f64]) -> Option<f64> {
    series
        .iter()
        .rev()
        .find(|x| !x.is_nan())
        .map(|x| py_round(*x, 4))
}
