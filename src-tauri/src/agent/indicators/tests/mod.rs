//! Tests of the Agent's indicators against what OpenAlgo web answers.
//!
//! The fixtures under `tests/fixtures/agent_indicators/` are recorded by
//! `scripts/gen_agent_indicator_goldens.py`, which runs the web's own
//! `services/agent/indicators` code over the openalgo version the web pins.
//!
//! | Module | What it pins |
//! | --- | --- |
//! | `golden_raw` | every `ta` method's numbers and errors, on five candle shapes |
//! | `golden_compute` | the dispatcher: results, refusals, sizing, rounding, scan parser |
//! | `catalogue` | the registry, the rendered specs, search and the discovery refusals |
//! | `arguments` | the web's own argument tests, the batch and the scan core |

mod arguments;
mod catalogue;
mod golden_compute;
mod golden_raw;
pub(crate) mod support;
