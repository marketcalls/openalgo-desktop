//! Trend (20), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, C, CV, HL, HLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "sma",
        category: "trend",
        inputs: C,
        outputs: &["sma"],
        params: &[ParamSpec::required_int("period")],
        warmup: 13,
        note: "period has no default: the caller must supply it or the call raises TypeError. Warm-up is period-1.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ema",
        category: "trend",
        inputs: C,
        outputs: &["ema"],
        params: &[ParamSpec::required_int("period")],
        note: "period has no default: the caller must supply it or the call raises TypeError. No warm-up: seeded from bar 0.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "wma",
        category: "trend",
        inputs: C,
        outputs: &["wma"],
        params: &[ParamSpec::required_int("period")],
        warmup: 13,
        note: "period has no default: the caller must supply it or the call raises TypeError. Warm-up is period-1.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "dema",
        category: "trend",
        inputs: C,
        outputs: &["dema"],
        params: &[ParamSpec::required_int("period")],
        warmup: 26,
        note: "period has no default: the caller must supply it or the call raises TypeError. Warm-up is about 2*period-2.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "tema",
        category: "trend",
        inputs: C,
        outputs: &["tema"],
        params: &[ParamSpec::required_int("period")],
        warmup: 39,
        note: "period has no default: the caller must supply it or the call raises TypeError. Warm-up is about 3*period-3.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "hma",
        category: "trend",
        inputs: C,
        outputs: &["hma"],
        params: &[ParamSpec::required_int("period")],
        warmup: 15,
        note: "period has no default: the caller must supply it or the call raises TypeError.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "vwma",
        category: "trend",
        inputs: CV,
        outputs: &["vwma"],
        params: &[ParamSpec::required_int("period")],
        warmup: 13,
        note: "Takes close and volume. period has no default: the caller must supply it or the call raises TypeError.",
        series_args: &["data", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "alma",
        category: "trend",
        inputs: C,
        outputs: &["alma"],
        params: &[ParamSpec::int("period", 21), ParamSpec::float("offset", 0.85), ParamSpec::float("sigma", 6.0)],
        warmup: 20,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "kama",
        category: "trend",
        inputs: C,
        outputs: &["kama"],
        params: &[ParamSpec::int("length", 14), ParamSpec::int("fast_length", 2), ParamSpec::int("slow_length", 30)],
        warmup: 14,
        note: "Period argument is named length, not period.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "zlema",
        category: "trend",
        inputs: C,
        outputs: &["zlema"],
        params: &[ParamSpec::required_int("period")],
        note: "period has no default: the caller must supply it or the call raises TypeError.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "t3",
        category: "trend",
        inputs: C,
        outputs: &["t3"],
        params: &[ParamSpec::int("period", 21), ParamSpec::float("v_factor", 0.7)],
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "frama",
        category: "trend",
        inputs: HL,
        outputs: &["frama"],
        params: &[ParamSpec::int("period", 26)],
        note: "TRAP: takes high and low, NOT close, unlike every other moving average.",
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "trima",
        category: "trend",
        inputs: C,
        outputs: &["trima"],
        params: &[ParamSpec::int("period", 20)],
        warmup: 19,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "mcginley",
        category: "trend",
        inputs: C,
        outputs: &["mcginley"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "vidya",
        category: "trend",
        inputs: C,
        outputs: &["vidya"],
        params: &[ParamSpec::int("period", 14), ParamSpec::float("alpha", 0.2)],
        warmup: 14,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "alligator",
        category: "trend",
        inputs: C,
        outputs: &["jaw", "teeth", "lips"],
        params: &[ParamSpec::int("jaw_period", 13), ParamSpec::int("jaw_shift", 8), ParamSpec::int("teeth_period", 8), ParamSpec::int("teeth_shift", 5), ParamSpec::int("lips_period", 5), ParamSpec::int("lips_shift", 3)],
        warmup: 20,
        note: "Shift arguments displace the lines; warm-up differs per line (20/12/7).",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ma_envelopes",
        category: "trend",
        inputs: C,
        outputs: &["upper_envelope", "middle_line", "lower_envelope"],
        params: &[ParamSpec::int("period", 20), ParamSpec::float("percentage", 2.5), ParamSpec::text("ma_type", "SMA").allowed(&["SMA", "EMA"])],
        warmup: 19,
        note: "ma_type is validated: SMA or EMA only.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "supertrend",
        category: "trend",
        inputs: HLC,
        outputs: &["supertrend", "direction"],
        params: &[ParamSpec::int("period", 10), ParamSpec::float("multiplier", 3.0)],
        warmup: 9,
        note: "direction is float64 and carries the Pine convention, which is the opposite of what the sign reads like: -1.0 means the trend is UP and the supertrend line sits BELOW price, +1.0 means the trend is DOWN and the line sits above it. Report -1 as bullish. It is not a bool. Arguments are period then multiplier.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ichimoku",
        category: "trend",
        inputs: HLC,
        outputs: &["conversion_line", "base_line", "leading_span_a", "leading_span_b", "lagging_span"],
        params: &[ParamSpec::int("conversion_periods", 9), ParamSpec::int("base_periods", 26), ParamSpec::int("lagging_span2_periods", 52), ParamSpec::int("displacement", 26)],
        warmup: 76,
        note: "Per-line warm-up is 8/25/50/76/0; the spans are displaced forward.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ckstop",
        category: "trend",
        inputs: HLC,
        outputs: &["stop_long", "stop_short"],
        params: &[ParamSpec::int("p", 10), ParamSpec::float("x", 1.0), ParamSpec::int("q", 9)],
        warmup: 17,
        note: "Chande Kroll Stop. Arguments are p (atr period), x (multiplier), q (stop period).",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
];
