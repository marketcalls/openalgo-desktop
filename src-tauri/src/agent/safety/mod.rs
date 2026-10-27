//! The Agent's order safety (the web's `services/agent/safety/` and the
//! risk half of `services/agent/settings.py`).
//!
//! What lives where:
//!
//! - [`risk`]: the order guard, pure: the eleven rules, the 24 codes,
//!   verdicts, the session book's claim.
//! - [`limits`]: the trader's trading and risk settings as the guard reads
//!   them, fresh for every order.
//! - [`kill_switch`]: the kill switch file.
//! - [`counters`]: the session books, persisted (migration
//!   `099_agent_safety`), checked and claimed in one write transaction.
//! - [`guard`]: the guard as the order tools call it.
//! - [`audit`]: the audit trail in `logs.db` (logs migration
//!   `014_agent_audit`) and its read.
//! - [`approval`]: the approval hook, each decision in the audit trail.
//! - [`previews`]: the position a smart order's approval card showed, so a
//!   position that moved since is never traded on (migration
//!   `099_agent_safety`).

pub mod approval;
pub mod audit;
pub mod counters;
pub mod guard;
pub mod kill_switch;
pub mod limits;
pub mod previews;
pub mod risk;

/// Migration `099_agent_safety`: the session counters and the positions
/// approval cards showed.
pub fn migrate(conn: &rusqlite::Connection) -> crate::error::Result<()> {
    counters::migrate(conn)?;
    previews::migrate(conn)
}
