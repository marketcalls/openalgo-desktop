//! Statistical (9), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, C, HLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "linreg",
        category: "statistical",
        inputs: C,
        outputs: &["linreg"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "lrslope",
        category: "statistical",
        inputs: C,
        outputs: &["lrslope"],
        params: &[ParamSpec::int("period", 100), ParamSpec::int("interval", 1)],
        warmup: 100,
        note: "period=100 by default, so it needs about 101 bars before the first value.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "correlation",
        category: "statistical",
        inputs: C,
        outputs: &["correlation"],
        params: &[ParamSpec::int("period", 20)],
        warmup: 19,
        note: "Takes TWO independent price series (rolling Pearson correlation). Cannot be driven from one OHLCV frame: the caller must fetch and align a second symbol.",
        series_args: &["data1", "data2"],
        needs_second_series: true,
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "beta",
        category: "statistical",
        inputs: C,
        outputs: &["beta"],
        params: &[ParamSpec::int("period", 252)],
        warmup: 252,
        note: "Takes TWO independent price series (asset against benchmark). period=252 means it needs about 253 bars; fewer than period bars raises ValueError.",
        series_args: &["asset", "market"],
        needs_second_series: true,
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "variance",
        category: "statistical",
        inputs: C,
        outputs: &["variance"],
        params: &[ParamSpec::int("lookback", 20), ParamSpec::text("mode", "PR").allowed(&["PR", "LR"]), ParamSpec::int("ema_period", 20), ParamSpec::int("filter_lookback", 20), ParamSpec::int("ema_length", 14), ParamSpec::flag("return_components", false)],
        warmup: 19,
        note: "mode is validated: PR or LR. Returns a 5-tuple when return_components=True.",
        series_args: &["data"],
        conditional_outputs: &[("return_components=True", &["variance", "ema_variance", "zscore", "ema_zscore", "stdev"])],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "tsf",
        category: "statistical",
        inputs: C,
        outputs: &["tsf"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "median",
        category: "statistical",
        inputs: C,
        outputs: &["median"],
        params: &[ParamSpec::int("period", 3)],
        warmup: 2,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "median_bands",
        category: "statistical",
        inputs: HLC,
        outputs: &["median", "upper_band", "lower_band", "median_ema"],
        params: &[ParamSpec::int("median_length", 3), ParamSpec::int("atr_length", 14), ParamSpec::float("atr_mult", 2.0)],
        warmup: 13,
        note: "Works on this build despite services/indicator_service.py listing it unsupported. source is an optional 4th positional override series (default hl2), so pass the tuning arguments by keyword or an int binds silently to source.",
        series_args: &["high", "low", "close", "source"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "mode",
        category: "statistical",
        inputs: C,
        outputs: &["mode"],
        params: &[ParamSpec::int("period", 20), ParamSpec::int("bins", 10)],
        warmup: 19,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
];
