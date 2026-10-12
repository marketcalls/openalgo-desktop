//! What OpenAlgo still manages, or may still owe, in Live (LOG-07, the
//! broker switch guard).
//!
//! A switch to another broker must not leave automation holding positions
//! it can no longer manage, nor an order whose fate is unknown. This counts
//! the open Live obligations on the connected broker so the switch can be
//! refused with a reason that names them (docs/codex-evaluation
//! 90-plus-roadmap, "Broker switching"):
//!
//! * running Live strategy runs bound to that broker;
//! * running Live OpenScript runs;
//! * active Live scalping stops;
//! * uncertain placements (LOG-08): strategy orders still `unconfirmed`,
//!   scalping exit orders without a definite answer, OpenScript orders
//!   still `unconfirmed`, and scalping exit attempts that cannot be read
//!   back (counted, never assumed settled);
//! * partially filled exits still working.
//!
//! Live only (Sandbox runs keep running on the new broker's quotes).
//! Synchronous: SQLite reads and in-memory registries, nothing across an
//! await and no network call.

use crate::error::Result;
use crate::scalping::monitor::{ChildState, ExitAttempt};
use crate::scalping::store::MODE_LIVE;
use crate::state::AppState;
use crate::strategy::dispatch::RunMode;

/// Open Live obligations on one broker.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OpenObligations {
    pub strategy_runs: u32,
    pub openscript_runs: u32,
    pub scalping_stops: u32,
    pub uncertain_placements: u32,
    pub partial_exits: u32,
}

impl OpenObligations {
    /// Nothing is open: a switch leaves nothing unmanaged.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn n(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

fn working(status: &str) -> bool {
    crate::strategy::recovery::order_is_working(status)
}

/// The open Live obligations on `broker_id` (the broker connected now).
pub fn open_live_obligations(state: &AppState, broker_id: &str) -> Result<OpenObligations> {
    let mut o = OpenObligations::default();

    // Strategy runs: Live, open, opened on this broker (a run with no
    // recorded broker counts: it can only be this one's).
    let store = &state.strategy.store;
    for run in store.list_open_runs()? {
        if RunMode::parse(&run.mode) != Some(RunMode::Live) {
            continue;
        }
        let b = run.broker.trim();
        if !b.is_empty() && !b.eq_ignore_ascii_case(broker_id.trim()) {
            continue;
        }
        o.strategy_runs += 1;
        for order in store.list_orders(run.id)? {
            let has_id = order
                .broker_order_id
                .as_deref()
                .is_some_and(|b| !b.trim().is_empty());
            if !has_id && order.status.eq_ignore_ascii_case("unconfirmed") {
                o.uncertain_placements += 1;
            } else if order.kind != "entry"
                && has_id
                && working(&order.status)
                && order.filled_qty.unwrap_or(0) > 0
            {
                o.partial_exits += 1;
            }
        }
    }

    // OpenScript runs: Live runs in the registry.
    o.openscript_runs = n(state
        .trading
        .runner
        .running()
        .iter()
        .filter(|r| r.mode == RunMode::Live)
        .count());

    // Scalping: active Live stops, and the exit orders recorded on them.
    for sl in state.scalping.store.active_sl(Some(MODE_LIVE))? {
        o.scalping_stops += 1;
        let Some(text) = sl.exit_attempt.as_deref().filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        match serde_json::from_str::<ExitAttempt>(text) {
            Ok(a) => {
                o.uncertain_placements += n(a
                    .children
                    .iter()
                    .filter(|c| {
                        c.state == ChildState::Unconfirmed
                            || (c.state == ChildState::Working && c.order_id.is_none())
                    })
                    .count());
                if a.working() && a.filled() > 0 {
                    o.partial_exits += 1;
                }
            }
            // Exit orders that cannot be read back may be anywhere.
            Err(_) => o.uncertain_placements += 1,
        }
    }

    // OpenScript orders still without a definite answer, and exits partly
    // filled and still working. A read that fails refuses the switch.
    let conn = state.sqlite.conn()?;
    let (unconfirmed, partial): (i64, i64) = conn.query_row(
        "SELECT
            COALESCE(SUM(CASE WHEN status = 'unconfirmed' THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN is_exit = 1 AND filled_quantity > 0
                AND filled_quantity < quantity
                AND status IN ('placed', 'working', 'triggerPending') THEN 1 ELSE 0 END), 0)
         FROM openscript_orders WHERE mode = 'live'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    o.uncertain_placements += u32::try_from(unconfirmed.max(0)).unwrap_or(u32::MAX);
    o.partial_exits += u32::try_from(partial.max(0)).unwrap_or(u32::MAX);
    Ok(o)
}
