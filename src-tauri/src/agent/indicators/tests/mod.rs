//! Tests of the Agent's indicators against what OpenAlgo web answers.
//!
//! The fixtures under `tests/fixtures/agent_indicators/` are recorded by
//! `scripts/gen_agent_indicator_goldens.py`, which runs the web's own
//! `services/agent/indicators` code over openalgo 2.0.5, the build `oa_core`
//! is pinned to (the web pins 2.4.1), plus 2.4.1's `rvi_volatility`.
//!
//! | Module | What it pins |
//! | --- | --- |
//! | `golden_raw` | every `ta` method's numbers and errors, on five candle shapes |
//! | `golden_compute` | the dispatcher: results, refusals, sizing, rounding, scan parser |
//! | `catalogue` | the registry, the rendered specs, search and the discovery refusals |
//! | `arguments` | the web's own argument tests, the batch and the scan core |
//! | `readme` | the README's promise about the indicators: count and build |

mod arguments;
mod catalogue;
mod golden_compute;
mod golden_raw;
mod readme;
pub(crate) mod support;
