//! `from openalgo import ta` in Rust: the 127 callables the Agent's
//! indicator tools compute with, giving the numbers OpenAlgo web gives.
//!
//! The web runs the `openalgo` package its `uv.lock` pins (2.0.5). Its
//! indicator classes validate their arguments and then either call one
//! kernel of the compiled Rust core (`openalgo._oaindicators`, a thin PyO3
//! wrapper over `oa_core`) or compose several kernels with NumPy in
//! `indicators/_backend.py`. Here the kernels are the same `oa_core`
//! functions, pinned to the same commit, and each composition and each check
//! is ported line for line from the installed package, in the order the
//! Python runs it, so the first error raised is the same one and carries the
//! same sentence.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `numpy` | the NumPy operations whose result depends on NumPy's algorithm |
//! | `trend`, `momentum`, `volatility`, `volume`, `oscillators`, `statistics`, `hybrid`, `talib_extra` | one function per `ta` method, by the package's module |
//! | `utils` | the 13 helpers (`crossover`, `exrem`, `valuewhen`, ...) |
//! | here | the argument and error types, the shared checks, the dispatch |
//!
//! Golden values recorded from the web's installed package pin every method:
//! `src-tauri/tests/fixtures/agent_indicators/raw_*.json`.

mod hybrid;
mod momentum;
mod numpy;
mod oscillators;
mod statistics;
mod talib_extra;
mod trend;
mod utils;
mod volatility;
mod volume;

use crate::agent::tools::values::float_repr;

/// One output of an indicator: numbers (NaN where the indicator has no value
/// yet) or the boolean flags `fractals`, `rising`, `falling` and the cross
/// helpers return.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Series {
    Float(Vec<f64>),
    Bool(Vec<bool>),
}

/// A Python exception the method raised: its type name and its message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TaError {
    pub kind: &'static str,
    pub message: String,
}

impl TaError {
    pub(crate) fn value(message: impl Into<String>) -> Self {
        Self {
            kind: "ValueError",
            message: message.into(),
        }
    }

    pub(crate) fn type_error(message: impl Into<String>) -> Self {
        Self {
            kind: "TypeError",
            message: message.into(),
        }
    }

    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self {
            kind: "RuntimeError",
            message: message.into(),
        }
    }
}

pub(crate) type TaResult = Result<Vec<Series>, TaError>;

/// A kernel over high, low, close and a period (CCI, Williams %R, DX, ADXR).
type HlcKernel = fn(&[f64], &[f64], &[f64], usize) -> Vec<f64>;

/// A kernel over high, low, close, volume and a period (MFI, CMF).
type HlcvKernel = fn(&[f64], &[f64], &[f64], &[f64], usize) -> Vec<f64>;

/// One argument value after the dispatcher's coercion.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ParamValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl ParamValue {
    /// Python `str()` of the value, as an f-string prints it.
    pub(crate) fn display(&self) -> String {
        match self {
            ParamValue::Int(v) => v.to_string(),
            ParamValue::Float(v) => float_repr(*v),
            ParamValue::Bool(true) => "True".into(),
            ParamValue::Bool(false) => "False".into(),
            ParamValue::Str(s) => s.clone(),
        }
    }
}

/// The non-series arguments of one call: what the caller passed, and the
/// method's own default for the rest. A parameter with no default that the
/// caller left out is absent, as in Python, and reading it raises the
/// `TypeError` the call would.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    method: &'static str,
    /// The signature's names for the series arguments, for the `TypeError`
    /// a missing one raises.
    series: &'static [&'static str],
    values: Vec<(&'static str, ParamValue)>,
}

impl Params {
    pub(crate) fn new(method: &'static str, values: Vec<(&'static str, ParamValue)>) -> Self {
        Self {
            method,
            series: &[],
            values,
        }
    }

    /// The same parameters, with the method's series argument names.
    pub(crate) fn with_series(mut self, series: &'static [&'static str]) -> Self {
        self.series = series;
        self
    }

    fn get(&self, name: &str) -> Result<&ParamValue, TaError> {
        self.values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                TaError::type_error(format!(
                    "TechnicalAnalysis.{}() missing 1 required positional argument: '{}'",
                    self.method, name
                ))
            })
    }

    /// An integer argument.
    pub(crate) fn int(&self, name: &str) -> Result<i64, TaError> {
        match self.get(name)? {
            ParamValue::Int(v) => Ok(*v),
            ParamValue::Float(v) => Ok(*v as i64),
            ParamValue::Bool(v) => Ok(i64::from(*v)),
            ParamValue::Str(s) => Err(TaError::type_error(format!(
                "'{}' must be a number, got '{}'",
                name, s
            ))),
        }
    }

    /// A numeric argument read as a float, as Python arithmetic reads an int.
    pub(crate) fn float(&self, name: &str) -> Result<f64, TaError> {
        match self.get(name)? {
            ParamValue::Int(v) => Ok(*v as f64),
            ParamValue::Float(v) => Ok(*v),
            ParamValue::Bool(v) => Ok(f64::from(u8::from(*v))),
            ParamValue::Str(s) => Err(TaError::type_error(format!(
                "'{}' must be a number, got '{}'",
                name, s
            ))),
        }
    }

    /// A text argument.
    pub(crate) fn text(&self, name: &str) -> Result<&str, TaError> {
        match self.get(name)? {
            ParamValue::Str(s) => Ok(s),
            other => Err(TaError::type_error(format!(
                "'{}' must be text, got {}",
                name,
                other.display()
            ))),
        }
    }

    /// A flag, read with Python truthiness.
    pub(crate) fn flag(&self, name: &str) -> Result<bool, TaError> {
        Ok(match self.get(name)? {
            ParamValue::Bool(v) => *v,
            ParamValue::Int(v) => *v != 0,
            ParamValue::Float(v) => *v != 0.0,
            ParamValue::Str(s) => !s.is_empty(),
        })
    }

    /// The argument as an f-string prints it (`3.0`, `5`, `SMA`).
    pub(crate) fn shown(&self, name: &str) -> Result<String, TaError> {
        Ok(self.get(name)?.display())
    }
}

/// The series and parameters one method is called with.
pub(crate) struct Args<'a> {
    series: &'a [&'a [f64]],
    params: &'a Params,
}

impl<'a> Args<'a> {
    /// The `i`th positional series, or the `TypeError` Python raises when
    /// it is missing.
    fn s(&self, i: usize) -> Result<&'a [f64], TaError> {
        self.series.get(i).copied().ok_or_else(|| {
            let p = self.params;
            TaError::type_error(match p.series.get(i) {
                Some(name) => format!(
                    "TechnicalAnalysis.{}() missing 1 required positional argument: '{}'",
                    p.method, name
                ),
                None => format!(
                    "TechnicalAnalysis.{}() missing a required series argument",
                    p.method
                ),
            })
        })
    }

    /// The first `count` series, each checked non-empty and all one length:
    /// the wrappers' `validate_input` on each, then `align_arrays`.
    fn aligned<const N: usize>(&self) -> Result<[&'a [f64]; N], TaError> {
        let mut out: [&[f64]; N] = [&[]; N];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = non_empty(self.s(i)?)?;
        }
        align(&out)?;
        Ok(out)
    }

    fn p(&self) -> &'a Params {
        self.params
    }
}

/// `BaseIndicator.validate_input`: an empty series is refused.
fn non_empty(data: &[f64]) -> Result<&[f64], TaError> {
    if data.is_empty() {
        Err(TaError::value("Input data cannot be empty"))
    } else {
        Ok(data)
    }
}

/// `BaseIndicator.align_arrays`: every series must be the same length.
fn align(arrays: &[&[f64]]) -> Result<(), TaError> {
    let lengths: Vec<usize> = arrays.iter().map(|a| a.len()).collect();
    if lengths.windows(2).any(|w| w[0] != w[1]) {
        let shown: Vec<String> = lengths.iter().map(|l| l.to_string()).collect();
        return Err(TaError::value(format!(
            "All arrays must have the same length. Got lengths: [{}]",
            shown.join(", ")
        )));
    }
    Ok(())
}

/// `BaseIndicator.validate_period`.
fn validate_period(period: i64, data_length: usize) -> Result<(), TaError> {
    if period <= 0 {
        return Err(TaError::value(format!(
            "Period must be positive, got {}",
            period
        )));
    }
    if period as u64 > data_length as u64 {
        return Err(TaError::value(format!(
            "Period ({}) cannot be greater than data length ({})",
            period, data_length
        )));
    }
    Ok(())
}

/// A Python int handed to a kernel's `usize` argument (PyO3's conversion).
fn us(value: i64) -> Result<usize, TaError> {
    usize::try_from(value).map_err(|_| TaError {
        kind: "OverflowError",
        message: "can't convert negative int to unsigned".into(),
    })
}

fn one(values: Vec<f64>) -> TaResult {
    Ok(vec![Series::Float(values)])
}

fn many(values: Vec<Vec<f64>>) -> TaResult {
    Ok(values.into_iter().map(Series::Float).collect())
}

/// Run `ta.<name>(*series, **params)`. `None` when `ta` has no such method.
pub(crate) fn call(name: &str, series: &[&[f64]], params: &Params) -> Option<TaResult> {
    let args = Args { series, params };
    trend::call(name, &args)
        .or_else(|| momentum::call(name, &args))
        .or_else(|| volatility::call(name, &args))
        .or_else(|| volume::call(name, &args))
        .or_else(|| oscillators::call(name, &args))
        .or_else(|| statistics::call(name, &args))
        .or_else(|| hybrid::call(name, &args))
        .or_else(|| talib_extra::call(name, &args))
        .or_else(|| utils::call(name, &args))
}
