//! The shared, pure risk core (web `services/risk/`).
//!
//! One place decides whether a position has hit its stop, taken its target,
//! earned a tighter trailing stop, or whether a set of positions has run
//! past its combined limits. The strategy module, the scalping terminal and
//! any REST surface sit on it because of one rule:
//!
//! **No I/O of any kind.** No database, no broker, no market data, no clock,
//! no logging. Every input arrives as an argument and every decision leaves
//! as a return value. Consumers translate; they do not decide.
//!
//! The web's `test/risk/vectors.json` (copied to
//! `tests/fixtures/risk/vectors.json`) is the contract: every case must pass.
//!
//! * `models`    value types and input normalisation
//! * `position`  per-position stop, target and trailing stop
//! * `aggregate` combined stop, combined target, lock profit, trail to entry
//! * `adapters`  the legacy `evaluate_trail` dict shape

pub mod adapters;
pub mod aggregate;
pub mod models;
pub mod position;

#[cfg(test)]
mod tests;

pub use adapters::evaluate_trail;
pub use aggregate::{aggregate_pnl, evaluate_aggregate, position_pnl, trail_stops_to_entry};
pub use models::{
    as_price, format_price, is_price, normalise_side, side_from_quantity, stop_from_points,
    target_from_points, value_to_f64, AggregateDecision, AggregateRisk, BreachReason, PnLSummary,
    PositionDecision, PositionPnL, PositionRisk, Side, StopMove, TrailMode, TrailToEntryDecision,
    DEFAULT_TRAIL_TRIGGER,
};
pub use position::{evaluate_position, evaluate_position_state, validate_position};
