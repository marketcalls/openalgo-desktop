//! Which steps each parameter goes through in its `openalgo.ta` method
//! (see `python`): measured on openalgo 2.0.5 by giving every parameter
//! values of every type, `flow_indicators/types.json` pins the whole table.
//!
//! Three methods read a parameter differently by another parameter's value
//! (`steps_for`): `obv_smoothed` by `ma_type`, `variance` by
//! `return_components`, `pvi_with_signal` by `signal_type`. Boolean
//! parameters are not listed: every method reads them for their truth
//! value.

use serde_json::Value;

use super::python::Side::{Left as L, Right as R};
use super::python::Step::{self, *};
use super::IndicatorParams;
use crate::flow::contract::pyfmt::truthy;

/// `(method, parameter, steps)`; the mode-dependent ones are in
/// `steps_for`.
const RULES: &[(&str, &str, &[Step])] = &[
    ("accelerator_oscillator", "period", &[IntCall]),
    ("adx", "period", &[Period]),
    ("adxr", "period", &[IntCall]),
    ("alligator", "jaw_period", &[IntCall]),
    ("alligator", "jaw_shift", &[IntCall]),
    ("alligator", "teeth_period", &[IntCall]),
    ("alligator", "teeth_shift", &[IntCall]),
    ("alligator", "lips_period", &[IntCall]),
    ("alligator", "lips_shift", &[IntCall]),
    ("alma", "period", &[Period]),
    ("alma", "offset", &[Compare("<=", R)]),
    ("alma", "sigma", &[Compare("<=", L)]),
    ("apo", "fast_period", &[IntCall]),
    ("apo", "slow_period", &[IntCall]),
    ("apo", "ma_type", &[Upper]),
    ("aroon", "period", &[Period]),
    ("aroon_oscillator", "period", &[Period]),
    ("atr", "period", &[Period]),
    ("awesome_oscillator", "fast_period", &[IntCall]),
    ("awesome_oscillator", "slow_period", &[IntCall]),
    ("bbands", "period", &[Period]),
    ("bbands", "std_dev", &[Compare("<=", L)]),
    ("bbpercent", "period", &[Period]),
    ("bbpercent", "std_dev", &[ArrayMultiply]),
    ("bbwidth", "period", &[Period]),
    ("bbwidth", "std_dev", &[ArrayMultiply]),
    ("cci", "period", &[Period]),
    ("chaikin", "ema_period", &[IntCall]),
    ("chaikin", "roc_period", &[IntCall]),
    ("chandelier_exit", "period", &[Period]),
    ("chandelier_exit", "multiplier", &[ArrayMultiply]),
    ("change", "length", &[Index]),
    ("cho", "fast_period", &[IntCall]),
    ("cho", "slow_period", &[IntCall]),
    ("chop", "period", &[Period]),
    ("ckstop", "p", &[Period]),
    ("ckstop", "x", &[Compare("<=", L)]),
    ("ckstop", "q", &[Period]),
    ("cmf", "period", &[Period]),
    ("cmo", "period", &[Arith("+", L), Period]),
    ("coppock", "wma_length", &[IntCall]),
    ("coppock", "long_roc_length", &[IntCall]),
    ("coppock", "short_roc_length", &[IntCall]),
    ("crsi", "lenrsi", &[Compare(">", R)]),
    ("crsi", "lenupdown", &[Compare(">", L)]),
    ("crsi", "lenroc", &[Compare(">", L), Period]),
    ("dema", "period", &[Period]),
    ("dmi", "period", &[Period]),
    ("donchian", "period", &[Period]),
    ("dpo", "period", &[Period]),
    ("dx", "period", &[IntCall]),
    ("elderray", "period", &[IntCall]),
    ("ema", "period", &[Period]),
    ("emv", "length", &[Period]),
    ("emv", "divisor", &[Compare("<=", L)]),
    ("falling", "length", &[Index]),
    ("fisher", "length", &[Period]),
    ("force_index", "length", &[Period]),
    ("fractals", "periods", &[Compare("<", L)]),
    ("frama", "period", &[Period]),
    ("gator_oscillator", "jaw_period", &[IntCall]),
    ("gator_oscillator", "teeth_period", &[IntCall]),
    ("gator_oscillator", "lips_period", &[IntCall]),
    ("highest", "period", &[Arith("-", L)]),
    ("hma", "period", &[Period]),
    ("hv", "length", &[Arith("+", L), Period]),
    ("hv", "annual", &[Compare("<=", L)]),
    ("hv", "per", &[Compare("<=", L)]),
    ("ichimoku", "conversion_periods", &[Compare("<=", L)]),
    ("ichimoku", "base_periods", &[Compare("<=", L)]),
    ("ichimoku", "lagging_span2_periods", &[Compare("<=", L)]),
    ("ichimoku", "displacement", &[Arith("-", L), Slice]),
    ("kama", "length", &[Arith("+", L), Period]),
    ("kama", "fast_length", &[Compare("<=", L)]),
    ("kama", "slow_length", &[Compare("<=", L)]),
    ("keltner", "ema_period", &[Period]),
    ("keltner", "atr_period", &[Period]),
    ("keltner", "multiplier", &[Compare("<=", L)]),
    ("kst", "roclen1", &[Compare("<=", L)]),
    ("kst", "roclen2", &[Compare("<=", L)]),
    ("kst", "roclen3", &[Compare("<=", L)]),
    ("kst", "roclen4", &[Compare("<=", L)]),
    ("kst", "smalen1", &[Compare("<=", L)]),
    ("kst", "smalen2", &[Compare("<=", L)]),
    ("kst", "smalen3", &[Compare("<=", L)]),
    ("kst", "smalen4", &[Compare("<=", L)]),
    ("kst", "siglen", &[Compare("<=", L)]),
    ("kvo", "trig_len", &[Compare("<=", L)]),
    ("kvo", "fast_x", &[Compare("<=", L)]),
    ("kvo", "slow_x", &[Compare("<=", L)]),
    ("linreg", "period", &[Period]),
    ("linregangle", "period", &[IntCall]),
    ("linregintercept", "period", &[IntCall]),
    ("lowest", "period", &[Arith("-", L)]),
    ("lrslope", "period", &[Arith("+", L), Period]),
    ("lrslope", "interval", &[Compare("<=", L)]),
    ("ma_envelopes", "period", &[Period]),
    ("ma_envelopes", "percentage", &[Arith("/", L)]),
    ("ma_envelopes", "ma_type", &[Upper]),
    ("macd", "fast_period", &[Compare("<=", L)]),
    ("macd", "slow_period", &[Compare("<=", L)]),
    ("macd", "signal_period", &[Compare("<=", L)]),
    ("massindex", "length", &[IntCall]),
    ("mcginley", "period", &[Period]),
    ("median", "period", &[Period]),
    ("mfi", "period", &[Period]),
    ("midpoint", "period", &[IntCall]),
    ("midprice", "period", &[IntCall]),
    ("minus_dm", "period", &[IntCall]),
    ("mode", "period", &[Period]),
    ("mode", "bins", &[IntCall]),
    ("mom", "period", &[IntCall]),
    ("natr", "period", &[IntCall]),
    ("nvi_with_ema", "ema_length", &[IntCall]),
    (
        "obv_smoothed",
        "ma_type",
        &[TextOrRefuse("Unsupported ma_type: {}")],
    ),
    ("plus_dm", "period", &[IntCall]),
    ("po", "fast_period", &[IntCall]),
    ("po", "slow_period", &[IntCall]),
    ("po", "ma_type", &[Upper]),
    ("ppo", "fast_period", &[IntCall]),
    ("ppo", "slow_period", &[IntCall]),
    ("ppo", "signal_period", &[IntCall]),
    ("psar", "acceleration", &[Compare("<=", L)]),
    ("psar", "maximum", &[Compare("<=", L)]),
    ("pvi", "initial_value", &[ArrayAssign]),
    ("pvi_with_signal", "initial_value", &[ArrayAssign]),
    ("pvi_with_signal", "signal_type", &[Upper]),
    ("rising", "length", &[Index]),
    ("roc", "length", &[IntCall]),
    ("rocp", "period", &[IntCall]),
    ("rocr", "period", &[IntCall]),
    ("rocr100", "period", &[IntCall]),
    ("rsi", "period", &[Period]),
    ("rvi", "period", &[Period]),
    ("rvol", "period", &[Period]),
    ("rwi", "period", &[Period]),
    ("sma", "period", &[Period]),
    ("starc", "ma_period", &[IntCall]),
    ("starc", "atr_period", &[IntCall]),
    ("starc", "multiplier", &[ArrayMultiply]),
    ("stc", "fast_length", &[IntCall]),
    ("stc", "slow_length", &[IntCall]),
    ("stc", "cycle_length", &[IntCall]),
    ("stc", "d1_length", &[IntCall]),
    ("stc", "d2_length", &[IntCall]),
    ("stdev", "period", &[IntCall]),
    ("stochastic", "k_period", &[Period]),
    ("stochastic", "smooth_k", &[Compare("<=", L)]),
    ("stochastic", "d_period", &[Compare("<=", L)]),
    ("stochf", "fastk_period", &[IntCall]),
    ("stochf", "fastd_period", &[IntCall]),
    ("stochrsi", "rsi_period", &[Arith("+", L), Period]),
    ("stochrsi", "stoch_period", &[Arith("+", R), Period]),
    ("stochrsi", "k_period", &[IntCall]),
    ("stochrsi", "d_period", &[IntCall]),
    ("supertrend", "period", &[Period]),
    ("supertrend", "multiplier", &[Compare("<=", L)]),
    ("t3", "period", &[Period]),
    ("t3", "v_factor", &[FloatCall]),
    ("tema", "period", &[Period]),
    ("trima", "period", &[Period]),
    ("trix", "length", &[Period]),
    ("tsf", "period", &[Period]),
    ("tsi", "long_period", &[IntCall]),
    ("tsi", "short_period", &[IntCall]),
    ("tsi", "signal_period", &[IntCall]),
    ("ultimate_oscillator", "period1", &[IntCall]),
    ("ultimate_oscillator", "period2", &[IntCall]),
    ("ultimate_oscillator", "period3", &[IntCall]),
    ("uo_oscillator", "period1", &[IntCall]),
    ("uo_oscillator", "period2", &[IntCall]),
    ("uo_oscillator", "period3", &[IntCall]),
    ("variance", "lookback", &[Period]),
    (
        "variance",
        "mode",
        &[TextOrRefuse(
            "Mode must be 'LR' (Logarithmic Returns) or 'PR' (Price)",
        )],
    ),
    ("vidya", "period", &[Arith("+", L), Period]),
    ("vidya", "alpha", &[FloatCall]),
    ("volosc", "short_length", &[IntCall]),
    ("volosc", "long_length", &[IntCall]),
    ("vroc", "period", &[Period]),
    ("vwap", "anchor", &[TextOrDefault]),
    ("vwap", "source", &[TextOrDefault]),
    ("vwap", "stdev_mult_1", &[Unused]),
    ("vwap", "stdev_mult_2", &[Unused]),
    ("vwap", "stdev_mult_3", &[Unused]),
    ("vwap", "percent_mult_1", &[Unused]),
    ("vwap", "percent_mult_2", &[Unused]),
    ("vwap", "percent_mult_3", &[Unused]),
    ("vwma", "period", &[Period]),
    ("williams_r", "period", &[Period]),
    ("wma", "period", &[Period]),
    ("zlema", "period", &[Period]),
];

const PERIOD: &[Step] = &[Period];
const INT_CALL: &[Step] = &[IntCall];
const UNUSED: &[Step] = &[Unused];
const COMPARE: &[Step] = &[Compare("<=", L)];
const SMA_LENGTH: &[Step] = &[Arith("-", L), Index];
const EMA_LENGTH: &[Step] = &[Arith("+", L)];

/// The steps `param` of `method` goes through, given the call's other
/// parameters (`None` for a parameter not in the table: a flag).
pub(crate) fn steps_for(
    method: &str,
    param: &str,
    params: &IndicatorParams,
) -> Option<&'static [Step]> {
    let other = |key: &str| {
        params
            .pairs()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    };
    // Text, or `None` for anything else (a `NaN` written in the params
    // text is a float, though JSON keeps it as text).
    let text = |key: &str, default: &'static str| match other(key) {
        Some(Value::String(s)) if params.non_finite(key).is_none() => Some(s.clone()),
        None => Some(default.to_string()),
        Some(_) => None,
    };
    match (method, param) {
        // `ma_type` picks the smoothing; an unknown one is refused before
        // any length is read.
        ("obv_smoothed", "ma_length" | "bb_length" | "bb_mult") => {
            Some(match (text("ma_type", "None").as_deref(), param) {
                (Some("SMA" | "EMA" | "WMA"), "ma_length") => PERIOD,
                (Some("SMMA (RMA)" | "VWMA"), "ma_length") => INT_CALL,
                (Some("SMA + Bollinger Bands"), "bb_length") => PERIOD,
                (Some("SMA + Bollinger Bands"), "bb_mult") => COMPARE,
                _ => UNUSED,
            })
        }
        // The components (and their lengths) are computed only on request.
        ("variance", "ema_period" | "filter_lookback" | "ema_length") => {
            Some(match other("return_components") {
                Some(flag) if truthy(flag) => INT_CALL,
                _ => UNUSED,
            })
        }
        // The signal line's length: an EMA's smoothing factor, an SMA's
        // window; an unknown or non-text type is refused first.
        ("pvi_with_signal", "signal_length") => Some(
            match text("signal_type", "EMA")
                .map(|t| t.to_uppercase())
                .as_deref()
            {
                Some("SMA") => SMA_LENGTH,
                Some("EMA") => EMA_LENGTH,
                _ => UNUSED,
            },
        ),
        _ => RULES
            .iter()
            .find(|(m, p, _)| *m == method && *p == param)
            .map(|(_, _, steps)| *steps),
    }
}
