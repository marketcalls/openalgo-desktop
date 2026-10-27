//! TA-Lib style extras (18), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, C, HL, HLC, OHLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "mom",
        category: "talib_extra",
        inputs: C,
        outputs: &["mom"],
        params: &[ParamSpec::int("period", 10)],
        warmup: 10,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rocp",
        category: "talib_extra",
        inputs: C,
        outputs: &["rocp"],
        params: &[ParamSpec::int("period", 10)],
        warmup: 10,
        note: "Rate of change as a fraction. Distinct from the ta.roc utility, which returns an ndarray.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rocr",
        category: "talib_extra",
        inputs: C,
        outputs: &["rocr"],
        params: &[ParamSpec::int("period", 10)],
        warmup: 10,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rocr100",
        category: "talib_extra",
        inputs: C,
        outputs: &["rocr100"],
        params: &[ParamSpec::int("period", 10)],
        warmup: 10,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "midpoint",
        category: "talib_extra",
        inputs: C,
        outputs: &["midpoint"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "apo",
        category: "talib_extra",
        inputs: C,
        outputs: &["apo"],
        params: &[ParamSpec::int("fast_period", 12), ParamSpec::int("slow_period", 26), ParamSpec::text("ma_type", "SMA")],
        warmup: 25,
        note: "ma_type is documented as SMA or EMA but is NOT validated: junk is accepted silently.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "medprice",
        category: "talib_extra",
        inputs: HL,
        outputs: &["medprice"],
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "typprice",
        category: "talib_extra",
        inputs: HLC,
        outputs: &["typprice"],
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "wclprice",
        category: "talib_extra",
        inputs: HLC,
        outputs: &["wclprice"],
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "midprice",
        category: "talib_extra",
        inputs: HL,
        outputs: &["midprice"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "avgprice",
        category: "talib_extra",
        inputs: OHLC,
        outputs: &["avgprice"],
        note: "Takes full OHLC; the open series parameter is named open_prices.",
        series_args: &["open_prices", "high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "plus_dm",
        category: "talib_extra",
        inputs: HL,
        outputs: &["plus_dm"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "minus_dm",
        category: "talib_extra",
        inputs: HL,
        outputs: &["minus_dm"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "dx",
        category: "talib_extra",
        inputs: HLC,
        outputs: &["dx"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 14,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "adxr",
        category: "talib_extra",
        inputs: HLC,
        outputs: &["adxr"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 40,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "stochf",
        category: "talib_extra",
        inputs: HLC,
        outputs: &["fastk", "fastd"],
        params: &[ParamSpec::int("fastk_period", 5), ParamSpec::int("fastd_period", 3)],
        warmup: 6,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "linregangle",
        category: "talib_extra",
        inputs: C,
        outputs: &["linregangle"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "linregintercept",
        category: "talib_extra",
        inputs: C,
        outputs: &["linregintercept"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
];
