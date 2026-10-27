//! Utilities (13), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, C, NONE};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "crossover",
        category: "utility",
        inputs: NONE,
        outputs: &["crossover"],
        note: "Utility function taking TWO derived series, which one instrument's candles cannot supply, so it has no callable form through these tools. Use scan_symbols, whose condition accepts crossover(a, b), crossunder(a, b) and cross(a, b) over an indicator's own outputs: that is what this primitive is for. True on the bar where series1 crosses ABOVE series2.",
        series_args: &["series1", "series2"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "crossunder",
        category: "utility",
        inputs: NONE,
        outputs: &["crossunder"],
        note: "Utility function taking TWO derived series, which one instrument's candles cannot supply, so it has no callable form through these tools. Use scan_symbols, whose condition accepts crossover(a, b), crossunder(a, b) and cross(a, b) over an indicator's own outputs: that is what this primitive is for. True on the bar where series1 crosses BELOW series2.",
        series_args: &["series1", "series2"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "cross",
        category: "utility",
        inputs: NONE,
        outputs: &["cross"],
        note: "Utility function taking TWO derived series, which one instrument's candles cannot supply, so it has no callable form through these tools. Use scan_symbols, whose condition accepts crossover(a, b), crossunder(a, b) and cross(a, b) over an indicator's own outputs: that is what this primitive is for. True on a cross in either direction.",
        series_args: &["series1", "series2"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "highest",
        category: "utility",
        inputs: C,
        outputs: &["highest"],
        params: &[ParamSpec::required_int("period")],
        warmup: 13,
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. Use donchian for the high-low channel. period is required (no default); warm-up is period-1.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "lowest",
        category: "utility",
        inputs: C,
        outputs: &["lowest"],
        params: &[ParamSpec::required_int("period")],
        warmup: 13,
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. Use donchian for the high-low channel. period is required (no default); warm-up is period-1.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "change",
        category: "utility",
        inputs: C,
        outputs: &["change"],
        params: &[ParamSpec::int("length", 1)],
        warmup: 1,
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. close[i] - close[i-length]. Use mom for the Series-returning form.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "roc",
        category: "utility",
        inputs: C,
        outputs: &["roc"],
        params: &[ParamSpec::required_int("length")],
        warmup: 14,
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. This is the UTILITY roc: length is required, the result is an ndarray and the warm-up is length (not length-1). The Series-returning rate-of-change family is mom/rocp/rocr/rocr100.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "stdev",
        category: "utility",
        inputs: C,
        outputs: &["stdev"],
        params: &[ParamSpec::required_int("period")],
        warmup: 13,
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. Use bbwidth for a normalised spread. period is required (no default); warm-up is period-1.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "exrem",
        category: "utility",
        inputs: NONE,
        outputs: &["exrem"],
        note: "Utility function taking TWO derived series, which one instrument's candles cannot supply, so it has no callable form through these tools. Use scan_symbols, whose condition accepts crossover(a, b), crossunder(a, b) and cross(a, b) over an indicator's own outputs: that is what this primitive is for. Removes repeated primary signals until a secondary signal fires.",
        series_args: &["primary", "secondary"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "flip",
        category: "utility",
        inputs: NONE,
        outputs: &["flip"],
        note: "Utility function taking TWO derived series, which one instrument's candles cannot supply, so it has no callable form through these tools. Use scan_symbols, whose condition accepts crossover(a, b), crossunder(a, b) and cross(a, b) over an indicator's own outputs: that is what this primitive is for. Latching toggle: on at primary, off at secondary.",
        series_args: &["primary", "secondary"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "valuewhen",
        category: "utility",
        inputs: NONE,
        outputs: &["valuewhen"],
        params: &[ParamSpec::int("n", 1)],
        note: "Utility function taking TWO derived series, which one instrument's candles cannot supply, so it has no callable form through these tools. Use scan_symbols, whose condition accepts crossover(a, b), crossunder(a, b) and cross(a, b) over an indicator's own outputs: that is what this primitive is for. Value of array at the nth most recent bar where expr was true. Warm-up is data dependent, not fixed: NaN until expr has fired n times.",
        series_args: &["expr", "array"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rising",
        category: "utility",
        inputs: C,
        outputs: &["rising"],
        params: &[ParamSpec::required_int("length")],
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. Returns BOOLEAN flags. Use lrslope for a gradient. length is required (no default).",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "falling",
        category: "utility",
        inputs: C,
        outputs: &["falling"],
        params: &[ParamSpec::required_int("length")],
        note: "Utility function: it takes a plain series rather than a named OHLCV role, and always returns a raw np.ndarray, so the pandas index is lost. Run on the close series here. Returns BOOLEAN flags. Use lrslope for a gradient. length is required (no default).",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
];
