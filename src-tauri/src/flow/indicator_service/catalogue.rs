//! Which `ta` names the Indicator node runs, and which series each one is
//! called with: `list_supported_indicators`, `is_single_series_indicator`,
//! `resolve_indicator_inputs` and the two refusal sets of the web's module.
//!
//! The web introspects `inspect.signature` of the installed `openalgo.ta`;
//! the Agent's catalogue records the same signatures (its `series_args`, in
//! call order), so the answers here are read from it.

use crate::agent::indicators::{catalogue as library, get_spec, IndicatorSpec};

/// `TWO_SERIES_INDICATORS`: indicators that need a second symbol or two
/// already-computed signals, which a single-symbol node cannot supply.
pub const TWO_SERIES_INDICATORS: [&str; 8] = [
    "correlation",
    "beta",
    "crossover",
    "crossunder",
    "cross",
    "exrem",
    "flip",
    "valuewhen",
];

/// `UNSUPPORTED_INDICATORS`: names the web refuses as returning no usable
/// value. Kept as the web has it (see the lane report: the Agent computes
/// `vi` and `ulcerindex` by a 1.0.4 decision; whether Flow should too is the
/// lead's call).
pub const UNSUPPORTED_INDICATORS: [&str; 3] = ["median_bands", "ulcerindex", "vi"];

/// `_SERIES_PARAM_TO_COLUMN`: the history column each series argument name
/// reads.
const SERIES_PARAM_TO_COLUMN: [(&str, &str); 7] = [
    ("open_prices", "open"),
    ("open", "open"),
    ("high", "high"),
    ("low", "low"),
    ("close", "close"),
    ("data", "close"),
    ("volume", "volume"),
];

fn column_for(argument: &str) -> Option<&'static str> {
    SERIES_PARAM_TO_COLUMN
        .iter()
        .find(|(name, _)| *name == argument)
        .map(|(_, column)| *column)
}

/// The name as the web's module keys it: `name.strip().lower()`.
pub(crate) fn normalise(name: &str) -> String {
    name.trim().to_lowercase()
}

/// The `ta` method a normalised name calls, when there is one.
pub(crate) fn spec(name: &str) -> Option<&'static IndicatorSpec> {
    get_spec(name)
}

/// `list_supported_indicators`: every `ta` name the single-symbol node can
/// run, sorted.
pub fn list_supported_indicators() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = library()
        .map(|spec| spec.name)
        .filter(|name| {
            !TWO_SERIES_INDICATORS.contains(name) && !UNSUPPORTED_INDICATORS.contains(name)
        })
        .collect();
    names.sort_unstable();
    names
}

/// `is_single_series_indicator`: true when the indicator's only series
/// argument is `data`, the one shape that can run on another indicator's
/// output series.
pub fn is_single_series_indicator(name: &str) -> bool {
    spec(&normalise(name)).is_some_and(takes_one_series)
}

pub(crate) fn takes_one_series(spec: &IndicatorSpec) -> bool {
    let mut series = spec
        .series_args
        .iter()
        .filter(|argument| column_for(argument).is_some());
    series.next() == Some(&"data") && series.next().is_none()
}

/// `resolve_indicator_inputs` with no explicit inputs: the history columns
/// the leading series arguments read, in call order (`close` when the
/// signature starts with none).
pub(crate) fn input_columns(spec: &IndicatorSpec) -> Vec<&'static str> {
    let columns: Vec<&'static str> = spec
        .series_args
        .iter()
        .map_while(|a| column_for(a))
        .collect();
    if columns.is_empty() {
        vec!["close"]
    } else {
        columns
    }
}
