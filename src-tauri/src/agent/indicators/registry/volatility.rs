//! Volatility (15), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, C, HL, HLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "atr",
        category: "volatility",
        inputs: HLC,
        outputs: &["atr"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "bbands",
        category: "volatility",
        inputs: C,
        outputs: &["upper_band", "middle_band", "lower_band"],
        params: &[
            ParamSpec::int("period", 20),
            ParamSpec::float("std_dev", 2.0),
        ],
        warmup: 19,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "keltner",
        category: "volatility",
        inputs: HLC,
        outputs: &["upper_channel", "middle_line", "lower_channel"],
        params: &[
            ParamSpec::int("ema_period", 20),
            ParamSpec::int("atr_period", 10),
            ParamSpec::float("multiplier", 2.0),
        ],
        warmup: 19,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "donchian",
        category: "volatility",
        inputs: HL,
        outputs: &["upper_channel", "middle_line", "lower_channel"],
        params: &[ParamSpec::int("period", 20)],
        warmup: 19,
        note: "Takes high and low only, no close.",
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "chaikin",
        category: "volatility",
        inputs: HL,
        outputs: &["chaikin"],
        params: &[
            ParamSpec::int("ema_period", 10),
            ParamSpec::int("roc_period", 10),
        ],
        warmup: 19,
        note: "Chaikin Volatility: high and low only, not the Chaikin oscillator (see ta.cho).",
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "natr",
        category: "volatility",
        inputs: HLC,
        outputs: &["natr"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ultimate_oscillator",
        category: "volatility",
        inputs: HLC,
        outputs: &["ultimate_oscillator"],
        params: &[
            ParamSpec::int("period1", 7),
            ParamSpec::int("period2", 14),
            ParamSpec::int("period3", 28),
        ],
        warmup: 27,
        note: "Numerically identical to ta.uo_oscillator; this is the canonical one.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "true_range",
        category: "volatility",
        inputs: HLC,
        outputs: &["true_range"],
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "massindex",
        category: "volatility",
        inputs: HL,
        outputs: &["massindex"],
        params: &[ParamSpec::int("length", 10)],
        warmup: 9,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "bbpercent",
        category: "volatility",
        inputs: C,
        outputs: &["bbpercent"],
        params: &[
            ParamSpec::int("period", 20),
            ParamSpec::float("std_dev", 2.0),
        ],
        warmup: 19,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "bbwidth",
        category: "volatility",
        inputs: C,
        outputs: &["bbwidth"],
        params: &[
            ParamSpec::int("period", 20),
            ParamSpec::float("std_dev", 2.0),
        ],
        warmup: 19,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "chandelier_exit",
        category: "volatility",
        inputs: HLC,
        outputs: &["long_exit", "short_exit"],
        params: &[
            ParamSpec::int("period", 22),
            ParamSpec::float("multiplier", 3.0),
        ],
        warmup: 21,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "hv",
        category: "volatility",
        inputs: C,
        outputs: &["hv"],
        params: &[
            ParamSpec::int("length", 10),
            ParamSpec::int("annual", 365),
            ParamSpec::int("per", 1),
        ],
        warmup: 10,
        note: "Historical volatility. Its close series parameter is literally named close.",
        series_args: &["close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ulcerindex",
        category: "volatility",
        inputs: C,
        outputs: &["ulcerindex"],
        params: &[
            ParamSpec::int("length", 14),
            ParamSpec::int("smooth_length", 14),
            ParamSpec::int("signal_length", 52),
            ParamSpec::text("signal_type", "SMA"),
            ParamSpec::flag("return_signal", false),
        ],
        warmup: 26,
        note: "signal_type is not validated by the library.",
        series_args: &["data"],
        conditional_outputs: &[("return_signal=True", &["ulcer", "signal"])],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "starc",
        category: "volatility",
        inputs: HLC,
        outputs: &["upper_band", "middle_line", "lower_band"],
        params: &[
            ParamSpec::int("ma_period", 5),
            ParamSpec::int("atr_period", 15),
            ParamSpec::float("multiplier", 1.33),
        ],
        warmup: 14,
        note: "Per-band warm-up is 14/4/14.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
];
