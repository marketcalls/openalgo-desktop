//! Volume (17), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, CV, HLCV, HLV, V};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "obv",
        category: "volume",
        inputs: CV,
        outputs: &["obv"],
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "obv_smoothed",
        category: "volume",
        inputs: CV,
        outputs: &["obv_smoothed"],
        params: &[ParamSpec::text("ma_type", "None").allowed(&["None", "SMA", "SMA + Bollinger Bands", "EMA", "SMMA (RMA)", "WMA", "VWMA"]), ParamSpec::int("ma_length", 20), ParamSpec::int("bb_length", 20), ParamSpec::float("bb_mult", 2.0)],
        note: "Returns a single series unless ma_type='SMA + Bollinger Bands', which yields a 3-tuple. ma_type is validated; an unknown value raises ValueError.",
        series_args: &["close", "volume"],
        conditional_outputs: &[("ma_type=SMA + Bollinger Bands", &["obv_smoothed", "bb_upper", "bb_lower"])],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "vwap",
        category: "volume",
        inputs: HLCV,
        outputs: &["vwap"],
        params: &[ParamSpec::text("anchor", "Session"), ParamSpec::text("source", "hlc3"), ParamSpec::float("stdev_mult_1", 1.0), ParamSpec::float("stdev_mult_2", 2.0), ParamSpec::float("stdev_mult_3", 3.0), ParamSpec::float("percent_mult_1", 0.236), ParamSpec::float("percent_mult_2", 0.382), ParamSpec::float("percent_mult_3", 0.618)],
        note: "Accepts six band-multiplier arguments but still returns a SINGLE series; the bands need ta._vwap.calculate_with_bands(). anchor and source are not validated.",
        series_args: &["high", "low", "close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "mfi",
        category: "volume",
        inputs: HLCV,
        outputs: &["mfi"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low", "close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "adl",
        category: "volume",
        inputs: HLCV,
        outputs: &["adl"],
        series_args: &["high", "low", "close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "cmf",
        category: "volume",
        inputs: HLCV,
        outputs: &["cmf"],
        params: &[ParamSpec::int("period", 20)],
        warmup: 19,
        series_args: &["high", "low", "close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "emv",
        category: "volume",
        inputs: HLV,
        outputs: &["emv"],
        params: &[ParamSpec::int("length", 14), ParamSpec::int("divisor", 10000)],
        warmup: 14,
        note: "TRAP: takes high, low and volume, with NO close.",
        series_args: &["high", "low", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "force_index",
        category: "volume",
        inputs: CV,
        outputs: &["force_index"],
        params: &[ParamSpec::int("length", 13)],
        warmup: 1,
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "nvi",
        category: "volume",
        inputs: CV,
        outputs: &["nvi"],
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "nvi_with_ema",
        category: "volume",
        inputs: CV,
        outputs: &["nvi", "nvi_ema"],
        params: &[ParamSpec::int("ema_length", 255)],
        note: "Same NVI class as ta.nvi; ema_length=255 means the EMA line needs a long history.",
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "pvi",
        category: "volume",
        inputs: CV,
        outputs: &["pvi"],
        params: &[ParamSpec::float("initial_value", 100.0)],
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "pvi_with_signal",
        category: "volume",
        inputs: CV,
        outputs: &["pvi", "signal"],
        params: &[ParamSpec::float("initial_value", 100.0), ParamSpec::text("signal_type", "EMA").allowed(&["EMA", "SMA"]), ParamSpec::int("signal_length", 255)],
        note: "signal_type is validated: EMA or SMA. signal_length=255 needs a long history for the signal line to be meaningful, even though both outputs are finite from bar 0.",
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "volosc",
        category: "volume",
        inputs: V,
        outputs: &["volosc"],
        params: &[ParamSpec::int("short_length", 5), ParamSpec::int("long_length", 10), ParamSpec::flag("check_volume_validity", true)],
        note: "TRAP: volume is its only series argument.",
        series_args: &["volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "vroc",
        category: "volume",
        inputs: V,
        outputs: &["vroc"],
        params: &[ParamSpec::int("period", 25)],
        warmup: 25,
        note: "TRAP: volume is its only series argument.",
        series_args: &["volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "kvo",
        category: "volume",
        inputs: HLCV,
        outputs: &["kvo", "trigger"],
        params: &[ParamSpec::int("trig_len", 13), ParamSpec::int("fast_x", 34), ParamSpec::int("slow_x", 55)],
        series_args: &["high", "low", "close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "pvt",
        category: "volume",
        inputs: CV,
        outputs: &["pvt"],
        series_args: &["close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rvol",
        category: "volume",
        inputs: V,
        outputs: &["rvol"],
        params: &[ParamSpec::int("period", 20)],
        warmup: 19,
        note: "TRAP: volume is its only series argument. Relative volume, not realized vol.",
        series_args: &["volume"],
        ..IndicatorSpec::BASE
    },
];
