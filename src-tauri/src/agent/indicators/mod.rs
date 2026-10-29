//! The Agent's technical indicators: the 127 `openalgo.ta` callables, with
//! the numbers OpenAlgo web gives, behind the pure functions the indicator
//! tools call. Ported from the web's `services/agent/indicators/` (registry,
//! descriptions, compute) and the pure parts of
//! `services/agent/tools/indicators.py`.
//!
//! Nothing here touches the network, the database or the clock: candles
//! arrive as the cleaned frame the tool fetched
//! ([`crate::agent::tools::market::candles::CandleFrame`]), and every
//! decision leaves as a return value, so the tools and the tests drive it
//! the same way. A refusal is the `ToolOutcome` the model reads, worded by
//! the shared `invalid_argument`.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `ta` | `from openalgo import ta` in Rust, on `oa_core` (the compiled core the web's package wraps) |
//! | `registry` | the catalogue: inputs, parameters, outputs, warm-up, caveats |
//! | `descriptions` | one searchable sentence per indicator |
//! | `coerce` | borrowed parameter names, JSON-to-library coercion, window sizing |
//! | `compute` | one indicator over a frame, with the web's refusals and summary |
//! | `discovery` | `list_indicators` and `describe_indicator` |
//! | `scan` | `scan_symbols`' condition parser and per-instrument evaluation |
//! | `json` | a JSON value that keeps the web's key order |
//!
//! Deviation from the web, by the lead's decision for 1.0.4: `vi` and
//! `ulcerindex` compute. The web refuses them as broken, a verdict measured
//! on openalgo 2.0.3; the 2.0.5 build the web pins computes both, and their
//! golden values are recorded from it.

mod coerce;
mod compute;
mod descriptions;
mod discovery;
mod json;
mod registry;
mod scan;
// The Flow Indicator node calls the library directly
// (`crate::flow::indicator_service`, the port of the web's
// `services/indicator_service.py`), without the Agent's coercion.
pub(crate) mod ta;

#[cfg(test)]
pub(crate) mod tests;

use crate::agent::tools::market::candles::CandleFrame;
use serde_json::{Map, Value};

pub(crate) use coerce::required_bars;
pub(crate) use compute::{compute, ist_stamp, Comparison, Computed};
pub(crate) use discovery::{describe_indicator, list_indicators, unknown_indicator};
pub(crate) use json::Json;
pub(crate) use registry::{
    all as catalogue, get_spec, required_default, Default as ParamDefault, IndicatorSpec, Kind,
};
pub(crate) use scan::{plan_scan, scan_symbol, ScanPlan};

/// What a batch computed and what it could not.
#[derive(Debug, Clone, Default)]
pub(crate) struct BatchOutcome {
    pub results: Vec<Computed>,
    /// `(indicator, reason)` for each indicator that could not run.
    pub failures: Vec<(String, String)>,
}

impl BatchOutcome {
    /// The refusal when nothing in the batch ran: every reason, joined.
    pub(crate) fn all_failed(&self) -> Option<String> {
        if !self.results.is_empty() {
            return None;
        }
        let reasons: Vec<&str> = self.failures.iter().map(|(_, e)| e.as_str()).collect();
        Some(reasons.join("; "))
    }
}

/// The body of `compute_indicators_batch` once the window is fetched: every
/// request over the same bars, one failure costing only its own entry.
pub(crate) fn compute_batch(
    frame: &CandleFrame,
    requests: &[(String, Map<String, Value>)],
    last_n: i64,
) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    for (name, params) in requests {
        match compute(name, frame, Some(params), None, last_n) {
            Ok(result) => outcome.results.push(result),
            Err(error) => outcome.failures.push((name.clone(), error.0)),
        }
    }
    outcome
}
