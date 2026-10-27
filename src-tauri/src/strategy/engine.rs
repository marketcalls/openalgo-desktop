//! The strategy engine: run lifecycle and the tick decision path (web
//! `services/strategy_module/engine.py`).
//!
//! ```text
//! start_run    resolve every leg, claim the strategy, place entries
//! stop_run     request exits and finalise after confirmed fills
//! close_leg    exit one leg; the run continues with the rest
//! process_tick decide risk on a price, then act on what was decided
//! apply_fill   record a fill against the exact position it belongs to
//! ```
//!
//! Each stage lives in its own child module: `start`, `fills`, `exits`,
//! `stop` and `ticks`.
//!
//! Load-bearing orderings, kept from the web:
//!
//! * Locks are released before orders are placed: evaluate under the lock,
//!   collect, release, then dispatch.
//! * Entries go BUY before SELL (a short leg alone can be refused for margin).
//! * Every leg is resolved before anything is claimed.
//! * An exit uses the symbol the run holds, never a re-resolved one.
//! * The order row is written before the broker is called. An entry that
//!   cannot be recorded is not placed; an exit that cannot be recorded is
//!   placed anyway (getting flat wins).
//! * A leg whose entry was accepted but not filled is never exited.
//! * A rejected exit releases its claim; a stop whose exits were refused
//!   leaves the run open and managed.
//! * A leg is closed by its fill arriving, not by its exit being placed.
//! * Trail-to-entry fires only on a stop-driven exit, never a manual close.

use super::dispatch::RunMode;
use super::store::EventFields;
use super::StrategyModule;
use crate::risk::BreachReason;
use serde_json::{json, Value};

mod exits;
mod fills;
pub(crate) use fills::FillOutcome;
mod start;
mod stop;
mod ticks;

/// What a start attempt produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StartResult {
    pub ok: bool,
    pub run_id: Option<i64>,
    pub error: Option<String>,
    /// Per-leg outcome, so a caller can say which leg failed and why.
    pub legs: Vec<Value>,
}

impl StartResult {
    fn fail(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Default::default()
        }
    }
}

/// A stop or close answer, in the web's dict shape.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StopOutcome {
    pub ok: bool,
    pub stop_pending: bool,
    pub error: Option<String>,
    pub exits: Vec<Value>,
    pub run_stopped: Option<bool>,
    /// The run or its strategy could not be read, so nothing was attempted
    /// (SM-04): an error to answer as such, never "not active" or "not
    /// found".
    pub read_failed: bool,
}

/// What a trader reads when a stop or a close could not read its own run.
pub const STOP_READ_FAILED: &str = "The strategy records could not be read just now, so nothing was closed and no exit order was placed. The run is still open and managed. Try again; if it keeps happening, restart the app.";

impl StopOutcome {
    fn err(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Default::default()
        }
    }

    /// A read of the run or its strategy that failed: logged with its
    /// cause, and answered as an error (SM-04).
    fn read_failed(what: std::fmt::Arguments<'_>, e: crate::error::AppError) -> Self {
        tracing::error!("Could not read {}: {}", what, e);
        Self {
            read_failed: true,
            ..Self::err(STOP_READ_FAILED)
        }
    }

    /// The HTTP status a refused stop or close answers with: 500 when its
    /// own records could not be read, else 409.
    pub fn error_status(&self) -> u16 {
        if self.read_failed {
            500
        } else {
            409
        }
    }

    fn pending(error: impl Into<String>, exits: Vec<Value>) -> Self {
        Self {
            ok: false,
            stop_pending: true,
            error: Some(error.into()),
            exits,
            ..Default::default()
        }
    }

    pub fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("ok".into(), json!(self.ok));
        m.insert("stop_pending".into(), json!(self.stop_pending));
        m.insert("exits".into(), json!(self.exits));
        if let Some(e) = &self.error {
            m.insert("error".into(), json!(e));
        }
        if let Some(r) = self.run_stopped {
            m.insert("run_stopped".into(), json!(r));
        }
        Value::Object(m)
    }
}

/// How a fill names the order and position it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct FillOpts {
    /// The quantity this fill adds (`None`: the whole owner).
    pub filled_qty: Option<i64>,
    pub order_row_id: Option<i64>,
    pub position_ref: Option<String>,
    pub cumulative_filled_qty: Option<i64>,
    pub order_terminal: bool,
    pub allow_prior_order_correction: bool,
}

impl Default for FillOpts {
    fn default() -> Self {
        Self {
            filled_qty: None,
            order_row_id: None,
            position_ref: None,
            cumulative_filled_qty: None,
            order_terminal: true,
            allow_prior_order_correction: false,
        }
    }
}

const UNFILLED_EXIT: &str = "The entry for this leg has been accepted but not filled, so there is no confirmed quantity to exit. Retry once it fills.";
/// Status of an order row whose placement had no definite answer (LOG-08).
pub const UNCONFIRMED: &str = "unconfirmed";
/// The witness event of an unconfirmed placement (carries the client tag).
pub const ORDER_UNCONFIRMED: &str = "order_unconfirmed";
const STATE_UNAVAILABLE: &str = "The run remains open because its live state is unavailable";

/// Which order kind a risk breach records.
fn exit_kind_for(reason: BreachReason) -> Option<&'static str> {
    match reason {
        BreachReason::Stop => Some("exit_sl"),
        BreachReason::Target => Some("exit_target"),
        BreachReason::CombinedStop => Some("exit_overall_sl"),
        BreachReason::CombinedTarget => Some("exit_overall_target"),
        BreachReason::LockProfit => Some("exit_lock_profit"),
    }
}

fn stop_reason_for(reason: BreachReason) -> Option<&'static str> {
    match reason {
        BreachReason::CombinedStop => Some("overall_sl"),
        BreachReason::CombinedTarget => Some("overall_target"),
        BreachReason::LockProfit => Some("lock_profit"),
        _ => None,
    }
}

fn ev(run_id: Option<i64>, leg_id: Option<i64>, severity: &'static str) -> EventFields {
    EventFields {
        run_id,
        leg_id,
        severity: Some(severity),
        payload: None,
    }
}

/// `(applied, remaining)` whole quantities for one owner's exit fill.
fn exit_fill_quantities(filled_qty: Option<i64>, held: i64) -> (i64, i64) {
    let held = held.max(0);
    let applied = match filled_qty {
        None => held,
        Some(q) => q.max(0).min(held),
    };
    (applied, held - applied)
}

fn rejection_summary(placed: &[Value]) -> String {
    let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
    for leg in placed {
        let reason = leg["error"].as_str().unwrap_or("").trim().to_string();
        if reason.is_empty() {
            continue;
        }
        let id = leg["leg_id"].to_string();
        match reasons.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, ids)) => ids.push(id),
            None => reasons.push((reason, vec![id])),
        }
    }
    match reasons.len() {
        0 => "Every entry order was rejected".into(),
        1 => format!("Every entry order was rejected: {}", reasons[0].0),
        _ => format!(
            "Every entry order was rejected. {}",
            reasons
                .iter()
                .map(|(r, ids)| format!("leg {}: {}", ids.join(", "), r))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

fn leg_id_of(leg: &Value, index: usize) -> i64 {
    leg.get("id")
        .or_else(|| leg.get("leg_id"))
        .and_then(Value::as_i64)
        .unwrap_or(index as i64 + 1)
}

impl StrategyModule {
    pub(crate) fn mode_of(&self, mode: &str) -> RunMode {
        RunMode::parse(mode).unwrap_or(RunMode::Sandbox)
    }
}
