//! Tests of the Flow indicator service against what OpenAlgo web answers.
//!
//! The fixtures under `tests/fixtures/flow_indicators/` are recorded by
//! `scripts/gen_flow_indicator_goldens.py`, which runs the web's own
//! `services/indicator_service.py` and executor nodes over openalgo 2.0.5.
//!
//! | Module | What it pins |
//! | --- | --- |
//! | `basics` | the API's own contract: parsing, tail and offset arithmetic |
//! | `golden_compute` | `compute_indicator`: every indicator, shape, refusal; IND-049 |
//! | `golden_bars` | `bar_at_offset` (IND-062) and `prior_period_ohlc` |
//! | `golden_window` | the bar and calendar ceilings |
//! | `golden_types` | every parameter given values of every type (IND-042) |
//! | `catalogue` | names, nesting, defaults, float binding, the editor's lists |
//! | `timestamps` | decision 33: the timestamp forms read and the ones refused (IND-045) |
//! | `executor` | the web executor's Indicator node, composed from this API |

mod basics;
mod catalogue;
mod executor;
mod golden_bars;
mod golden_compute;
mod golden_types;
mod golden_window;
mod support;
mod timestamps;
