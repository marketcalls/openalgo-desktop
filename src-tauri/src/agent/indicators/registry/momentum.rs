//! Momentum (9), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, C, HL, HLC, OHLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "rsi",
        category: "momentum",
        inputs: C,
        outputs: &["rsi"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 14,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "macd",
        category: "momentum",
        inputs: C,
        outputs: &["macd_line", "signal_line", "histogram"],
        params: &[ParamSpec::int("fast_period", 12), ParamSpec::int("slow_period", 26), ParamSpec::int("signal_period", 9)],
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "stochastic",
        category: "momentum",
        inputs: HLC,
        outputs: &["k_percent", "d_percent"],
        params: &[ParamSpec::int("k_period", 14), ParamSpec::int("smooth_k", 3), ParamSpec::int("d_period", 3)],
        warmup: 17,
        note: "TRAP: positional order is k_period, smooth_k, d_period. The docs omit smooth_k, so ta.stochastic(h, l, c, 14, 3) sets smooth_k, not d_period.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "cci",
        category: "momentum",
        inputs: HLC,
        outputs: &["cci"],
        params: &[ParamSpec::int("period", 20)],
        warmup: 19,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "williams_r",
        category: "momentum",
        inputs: HLC,
        outputs: &["williams_r"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "bop",
        category: "momentum",
        inputs: OHLC,
        outputs: &["bop"],
        note: "Takes full OHLC; the open series parameter is named open_prices.",
        series_args: &["open_prices", "high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "elderray",
        category: "momentum",
        inputs: HLC,
        outputs: &["bull_power", "bear_power"],
        params: &[ParamSpec::int("period", 13)],
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "fisher",
        category: "momentum",
        inputs: HL,
        outputs: &["fisher", "trigger"],
        params: &[ParamSpec::int("length", 9)],
        warmup: 8,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "crsi",
        category: "momentum",
        inputs: C,
        outputs: &["crsi"],
        params: &[ParamSpec::int("lenrsi", 3), ParamSpec::int("lenupdown", 2), ParamSpec::int("lenroc", 100)],
        warmup: 99,
        note: "Connors RSI. lenroc=100 means it needs about 100 bars before the first value.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
];
