//! Hybrid (7), in the web registry's order (`services/agent/indicators/registry.py`).

use super::{IndicatorSpec, ParamSpec, HL, HLC};

pub(super) const SPECS: &[IndicatorSpec] = &[
    IndicatorSpec {
        name: "adx",
        category: "hybrid",
        inputs: HLC,
        outputs: &["plus_di", "minus_di", "adx"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 26,
        note: "TRAP: ADX is the THIRD element, not the first. Per-line warm-up is 13/13/26.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "aroon",
        category: "hybrid",
        inputs: HL,
        outputs: &["aroon_up", "aroon_down"],
        params: &[ParamSpec::int("period", 25)],
        warmup: 25,
        note: "TRAP: default period is 25, not the 14 the hybrid doc claims.",
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "pivot_points",
        category: "hybrid",
        inputs: HLC,
        outputs: &["pivot", "r1", "s1", "r2", "s2", "r3", "s3"],
        note: "Seven outputs; the order interleaves resistance and support.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "psar",
        category: "hybrid",
        inputs: HL,
        outputs: &["psar"],
        params: &[ParamSpec::float("acceleration", 0.02), ParamSpec::float("maximum", 0.2)],
        note: "TRAP: returns a SINGLE series, not the (sar, direction) tuple the hybrid doc shows. Takes high and low only.",
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "dmi",
        category: "hybrid",
        inputs: HLC,
        outputs: &["plus_di", "minus_di"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 13,
        note: "Directional indicators only; use ta.adx for the ADX line.",
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "fractals",
        category: "hybrid",
        inputs: HL,
        outputs: &["fractal_up", "fractal_down"],
        params: &[ParamSpec::int("periods", 2)],
        note: "Williams fractals. Both outputs are BOOLEAN flags on this build, not the float 0.0/1.0 the docs suggest, so their summary reports how many bars fired rather than a minimum and a maximum.",
        series_args: &["high", "low"],
        ..IndicatorSpec::BASE
    },
    IndicatorSpec {
        name: "rwi",
        category: "hybrid",
        inputs: HLC,
        outputs: &["rwi_high", "rwi_low"],
        params: &[ParamSpec::int("period", 14)],
        warmup: 14,
        series_args: &["high", "low", "close"],
        ..IndicatorSpec::BASE
    },
];
