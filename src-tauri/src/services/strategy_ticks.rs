//! The strategy tick path's health. A tick the reader misses because the
//! market-data channel overran it is a price strategy risk management never
//! saw. The web counts its shed ticks only as a diagnostic; here the counts
//! are on the Health page (`/health/api/current`, `strategy_ticks`) and in
//! the system report, and a new miss raises a health alert (OBS-01 style).
//! An exit or stop decided and not placed within `SLOW_EXIT_AFTER` raises
//! one too, which OpenAlgo resolves once nothing waits that long; the web
//! has neither alert.

use crate::db::sqlite::monitor::{self as store, AlertRow};
use crate::error::Result;
use crate::state::AppState;
use crate::strategy::tick_feed::pending::{ExitWait, SLOW_EXIT_AFTER};
use crate::strategy::tick_feed::TickFeed;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::Value;

/// The alert a missed tick raises; the trader resolves it.
pub const ALERT: &str = "strategy_ticks_missed";

/// The alert a strategy exit waiting too long to be placed raises;
/// OpenAlgo resolves it once none does.
pub const SLOW_EXITS_ALERT: &str = "strategy_exits_slow";

/// Prices read, decisions combined and ticks missed since start, and the
/// strategies whose exits wait to be acted on.
pub fn json(ctx: &AppState) -> Value {
    ctx.strategy.feed.health(ctx.strategy.clock.now())
}

/// The tick path's alerts at `now` (the health sampler, once a minute).
pub fn alert(conn: &Connection, feed: &TickFeed, now: DateTime<Utc>) -> Result<()> {
    missed_prices_alert(conn, feed, now)?;
    slow_exits_alert(conn, feed.exit_wait(now), now)
}

/// Raise the alert when more ticks were missed since the last one.
fn missed_prices_alert(conn: &Connection, feed: &TickFeed, now: DateTime<Utc>) -> Result<()> {
    let Some(missed) = feed.ticks_missed_unreported() else {
        return Ok(());
    };
    store::raise_alert(
        conn,
        &AlertRow {
            alert_type: ALERT.into(),
            severity: "warn".into(),
            metric_name: ALERT.into(),
            metric_value: missed as f64,
            threshold_value: 0.0,
            message: format!(
                "Strategy risk management missed {} prices since OpenAlgo started: the market data came faster than it was read. Stops and targets act on the next price. If this keeps happening, hold fewer symbols or restart OpenAlgo, and report it.",
                missed
            ),
            ..Default::default()
        },
        now,
    )
}

/// Raise the slow-exit alert, with what waits now, while an exit or stop
/// has waited past `SLOW_EXIT_AFTER` to be placed; resolve it once none
/// has. The Strategy Prices card shows the same wait while the Health page
/// is open; the alert stays listed for a trader who was not looking.
fn slow_exits_alert(conn: &Connection, wait: ExitWait, now: DateTime<Utc>) -> Result<()> {
    let Some(waited_s) = wait.slow_for() else {
        store::auto_resolve(conn, SLOW_EXITS_ALERT, now)?;
        return Ok(());
    };
    store::raise_alert(
        conn,
        &AlertRow {
            alert_type: SLOW_EXITS_ALERT.into(),
            severity: "warn".into(),
            metric_name: SLOW_EXITS_ALERT.into(),
            metric_value: waited_s as f64,
            threshold_value: SLOW_EXIT_AFTER.as_secs() as f64,
            message: slow_exits_message(wait.strategies, waited_s),
            ..Default::default()
        },
        now,
    )
}

fn slow_exits_message(strategies: usize, waited_s: u64) -> String {
    let what = if strategies > 1 {
        format!(
            "{} strategies have an exit that is not placed yet; the oldest was decided {} seconds ago",
            strategies, waited_s
        )
    } else {
        format!(
            "A strategy exit decided {} seconds ago is not placed yet",
            waited_s
        )
    };
    format!(
        "{}: the broker or OpenAlgo's database is answering slowly. Check those positions with your broker. This alert clears itself once the exits are placed.",
        what
    )
}
