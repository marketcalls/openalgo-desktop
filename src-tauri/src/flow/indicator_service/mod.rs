//! The port of OpenAlgo web's `services/indicator_service.py`: what Flow's
//! Indicator, Bar Offset, Prior Period OHLC and History nodes compute with.
//!
//! Pure functions over the records `/api/v1/history` returns (its `data`
//! array: epoch-second timestamps, ascending). No route, no database, no
//! network and no clock: the caller passes "now" and "today" in IST, which is
//! the server-local time the web's code reads in its deployment. The numbers
//! come from the Agent's catalogue and its `oa_core` library
//! (`crate::agent::indicators`), called the way the web's dispatcher calls
//! `openalgo.ta`: the node's parameters reach the library as written, with
//! only `REQUIRED_PARAM_DEFAULTS` filled in.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `catalogue` | the names the node runs, the two-series and unsupported refusals, input columns |
//! | `params` | the node's `params` JSON, bound to the library as Python binds keyword arguments |
//! | `records` | history records read as the web's DataFrame reads them, timestamps in IST |
//! | `timestamps` | the timestamp column as `pd.to_datetime` reads it: whether it raises |
//! | `compute` | `compute_indicator`: nested mode, the fixed-length tail, the offset bar |
//! | `nested` | the executor's reading of `sourceSeries` into numbers |
//! | `window` | the bar and calendar ceilings: `clamp_bars`, `history_window`, `clamp_history_range`, `trim_records` |
//! | `bars` | `bar_at_offset` and `prior_period_ohlc` |
//!
//! Golden values recorded from the web's installed openalgo package pin
//! every function: `src-tauri/tests/fixtures/flow_indicators/`, written by
//! `scripts/gen_flow_indicator_goldens.py`.
//!
//! An error carries the Python exception the web raises ([`IndicatorError`]),
//! because the web's nodes differ in what they catch: the Indicator and Prior
//! Period OHLC nodes turn a `ValueError` into a node error and let anything
//! else stop the run; the Bar Offset node catches nothing.

mod bars;
mod catalogue;
mod compute;
mod nested;
mod params;
mod records;
mod timestamps;
mod window;

#[cfg(test)]
mod tests;

pub use bars::{bar_at_offset, prior_period_ohlc, PriorPeriodOhlc};
pub use catalogue::{
    is_single_series_indicator, list_supported_indicators, TWO_SERIES_INDICATORS,
    UNSUPPORTED_INDICATORS,
};
pub use compute::{
    compute_indicator, Computed, IndicatorOutput, IndicatorRequest, IndicatorSource, NoValue, Row,
};
pub use nested::{source_series_numbers, SourceSeriesError};
pub use params::IndicatorParams;
pub use window::{
    calendar_days_for_bars, clamp_bars, clamp_history_range, history_window, trim_records,
};

/// `MAX_HISTORY_BARS`: the deepest history any Flow node fetches or keeps.
/// The web reads `FLOW_MAX_HISTORY_BARS` (default 200); the desktop has no
/// environment settings, so the default is the value.
pub const MAX_HISTORY_BARS: i64 = 200;

/// `MAX_HISTORY_CALENDAR_DAYS`: the widest calendar span of one request
/// (`FLOW_MAX_HISTORY_CALENDAR_DAYS`, default 4000).
pub const MAX_HISTORY_CALENDAR_DAYS: i64 = 4000;

/// `_INDICATOR_WARMUP_BARS`: rows fed to an indicator beyond its lookback, so
/// recursive indicators have settled when the reported window starts.
const INDICATOR_WARMUP_BARS: i64 = 50;

/// An exception the web's code raises: its Python type and `str(e)`, the
/// sentence a node reports.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorError {
    pub kind: &'static str,
    pub message: String,
}

impl IndicatorError {
    pub(crate) fn value(message: impl Into<String>) -> Self {
        Self {
            kind: "ValueError",
            message: message.into(),
        }
    }

    pub(crate) fn type_error(message: impl Into<String>) -> Self {
        Self {
            kind: "TypeError",
            message: message.into(),
        }
    }

    /// True for a `ValueError` or one of the subclasses the web raises
    /// (pandas' `DateParseError` and `OutOfBoundsDatetime`): the kinds the
    /// web's Indicator and Prior Period OHLC nodes catch and report as a
    /// node error. Anything else stops the web's run.
    pub fn is_value_error(&self) -> bool {
        matches!(
            self.kind,
            "ValueError" | "DateParseError" | "OutOfBoundsDatetime"
        )
    }
}

impl std::fmt::Display for IndicatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
