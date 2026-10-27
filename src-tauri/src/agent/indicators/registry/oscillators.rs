//! Oscillators (19), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, Status, C, HL, HLC, HLCV, OHLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "cmo",
        category: "oscillators",
        inputs: C,
        outputs: &["cmo"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 14,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "trix",
        category: "oscillators",
        inputs: C,
        outputs: &["trix"],
        params: &[ParamSpec::int("length", 18)],
        warmup: 1,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "uo_oscillator",
        category: "oscillators",
        inputs: HLC,
        outputs: &["uo_oscillator"],
        params: &[ParamSpec::int("period1", 7), ParamSpec::int("period2", 14), ParamSpec::int("period3", 28)],
        status: Status::Alias,
        alias_of: Some("ultimate_oscillator"),
        warmup: 27,
        note: "oscillators.UO and volatility.ULTOSC are different classes with identical signatures and numerically identical output. Use ta.ultimate_oscillator instead.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "awesome_oscillator",
        category: "oscillators",
        inputs: HL,
        outputs: &["awesome_oscillator"],
        params: &[ParamSpec::int("fast_period", 5), ParamSpec::int("slow_period", 34)],
        warmup: 33,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "accelerator_oscillator",
        category: "oscillators",
        inputs: HL,
        outputs: &["accelerator_oscillator"],
        params: &[ParamSpec::int("period", 5)],
        warmup: 37,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "ppo",
        category: "oscillators",
        inputs: C,
        outputs: &["ppo_line", "signal_line", "histogram"],
        params: &[ParamSpec::int("fast_period", 12), ParamSpec::int("slow_period", 26), ParamSpec::int("signal_period", 9)],
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "po",
        category: "oscillators",
        inputs: C,
        outputs: &["po"],
        params: &[ParamSpec::int("fast_period", 10), ParamSpec::int("slow_period", 20), ParamSpec::text("ma_type", "SMA").allowed(&["SMA", "EMA"])],
        warmup: 19,
        note: "ma_type is validated: SMA or EMA only.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "dpo",
        category: "oscillators",
        inputs: C,
        outputs: &["dpo"],
        params: &[ParamSpec::int("period", 21), ParamSpec::flag("is_centered", false)],
        warmup: 31,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "aroon_oscillator",
        category: "oscillators",
        inputs: HL,
        outputs: &["aroon_oscillator"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 14,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "stochrsi",
        category: "oscillators",
        inputs: C,
        outputs: &["k_percent", "d_percent"],
        params: &[ParamSpec::int("rsi_period", 14), ParamSpec::int("stoch_period", 14), ParamSpec::int("k_period", 3), ParamSpec::int("d_period", 3)],
        warmup: 14,
        note: "The library calls these %K and %D; renamed here to stay identifier-safe.",
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rvi",
        category: "oscillators",
        inputs: OHLC,
        outputs: &["rvi", "signal"],
        params: &[ParamSpec::int("period", 10)],
        warmup: 15,
        note: "TRAP: this is the Relative VIGOR Index and needs full OHLC. The volatility doc's ta.rvi(df['close']) raises TypeError. The Relative Volatility Index is imported as VolatilityRVI and never exposed on ta.",
        series_args: &["open_prices", "high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "cho",
        category: "oscillators",
        inputs: HLCV,
        outputs: &["cho"],
        params: &[ParamSpec::int("fast_period", 3), ParamSpec::int("slow_period", 10)],
        note: "Chaikin oscillator, distinct from ta.chaikin (Chaikin volatility).",
        series_args: &["high", "low", "close", "volume"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "chop",
        category: "oscillators",
        inputs: HLC,
        outputs: &["chop"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "kst",
        category: "oscillators",
        inputs: C,
        outputs: &["kst", "signal_line"],
        params: &[ParamSpec::int("roclen1", 10), ParamSpec::int("roclen2", 15), ParamSpec::int("roclen3", 20), ParamSpec::int("roclen4", 30), ParamSpec::int("smalen1", 10), ParamSpec::int("smalen2", 10), ParamSpec::int("smalen3", 10), ParamSpec::int("smalen4", 15), ParamSpec::int("siglen", 9)],
        warmup: 52,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "tsi",
        category: "oscillators",
        inputs: C,
        outputs: &["tsi", "signal_line"],
        params: &[ParamSpec::int("long_period", 25), ParamSpec::int("short_period", 13), ParamSpec::int("signal_period", 13)],
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "vi",
        category: "oscillators",
        inputs: HLC,
        outputs: &["vi_plus", "vi_minus"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 14,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "stc",
        category: "oscillators",
        inputs: C,
        outputs: &["stc"],
        params: &[ParamSpec::int("fast_length", 23), ParamSpec::int("slow_length", 50), ParamSpec::int("cycle_length", 10), ParamSpec::int("d1_length", 3), ParamSpec::int("d2_length", 3)],
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "gator_oscillator",
        category: "oscillators",
        inputs: HL,
        outputs: &["upper_histogram", "lower_histogram"],
        params: &[ParamSpec::int("jaw_period", 13), ParamSpec::int("teeth_period", 8), ParamSpec::int("lips_period", 5)],
        warmup: 20,
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "coppock",
        category: "oscillators",
        inputs: C,
        outputs: &["coppock"],
        params: &[ParamSpec::int("wma_length", 10), ParamSpec::int("long_roc_length", 14), ParamSpec::int("short_roc_length", 11)],
        warmup: 14,
        series_args: &["data"],
        ..IndicatorSpec::BASE
    },
];
