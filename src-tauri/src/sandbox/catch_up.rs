//! Catch-up after the app was closed across a boundary (web
//! `sandbox/catch_up_processor.py`). Runs on engine start (analyzer mode on)
//! and whenever the API layer asks, single flight: a second trigger while one
//! runs is skipped, since the run in progress does the same work.
//!
//! Order:
//! 1. stale `today_realized_pnl` is zeroed on positions and funds, decided
//!    before anything below writes to the funds row (the web ran this after
//!    the MIS and T+1 steps, whose own funds writes made the row look fresh
//!    and skipped the reset);
//! 2. MIS positions untouched since the last session boundary are settled at
//!    their last price, the P&L booked to all-time realized only;
//! 3. expired F&O positions are settled;
//! 4. T+1 settlement of CNC positions created before today;
//! 5. yesterday's P&L snapshot is backfilled when it is missing (trading
//!    days, and any day the book held or traded crypto);
//! 6. GTT legs stranded mid-fire are reclaimed, expired GTTs released, and
//!    every active leg evaluated against one batch of quotes.

use super::clock::ts;
use super::config::SandboxConfig;
use super::core::{blocking, Core};
use super::events::Outbox;
use super::expiry;
use super::funds;
use super::gtt;
use super::holdings;
use super::positions;
use super::session;
use super::types::{dec_to_db, Product, SandboxError, SbResult};
use chrono::Duration;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use std::sync::Arc;

/// What a catch-up run did. `None` when skipped because one was running.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatchUpReport {
    pub stale_mis_settled: usize,
    pub expired_settled: usize,
    pub t1_settled: usize,
    pub pnl_rows_reset: usize,
    pub snapshot_backfilled: bool,
    pub gtt_reclaimed: usize,
    pub gtt_expired: usize,
    pub gtt_fired: usize,
}

/// Settle MIS positions left over from a previous session (web
/// `catch_up_mis_squareoff`).
pub(crate) fn settle_stale_mis(
    conn: &Connection,
    core: &Core,
    cfg: &SandboxConfig,
) -> rusqlite::Result<usize> {
    if core.opts.session_expiry_disabled {
        tracing::info!("Catch-up: no MIS session square-off for 24x7 markets");
        return Ok(0);
    }
    let now = core.now();
    let boundary = session::last_session_expiry(now, core.opts.session_expiry);
    let boundary_s = ts(boundary);
    let now_s = ts(now);
    let mut n = 0;
    for p in positions::list_open(conn, Some(Product::Mis))? {
        if p.updated_at.as_str() >= boundary_s.as_str() {
            continue;
        }
        if cfg.square_off_time(&p.exchange).is_none() {
            tracing::info!(
                "Catch-up: skipping {} on {} (no square-off time for this exchange)",
                p.symbol,
                p.exchange
            );
            continue;
        }
        // Claim with the boundary re-checked: a position traded since the
        // query is not a leftover.
        let claimed = conn.execute(
            "UPDATE sandbox_positions SET quantity = quantity
             WHERE id = ?1 AND quantity = ?2 AND updated_at < ?3",
            params![p.id, p.quantity, boundary_s],
        )?;
        if claimed != 1 {
            continue;
        }
        let Some(mut p) = positions::get_by_id(conn, p.id)? else {
            continue;
        };
        let settle = p
            .ltp
            .filter(|l| *l > Decimal::ZERO)
            .unwrap_or(p.average_price);
        let cv = core.contract_value(&p.symbol, &p.exchange);
        let pnl =
            positions::realized_pnl(p.quantity, p.average_price, p.quantity.abs(), settle, cv);
        let margin = p.margin_blocked;
        let user = p.user_id.clone();
        if funds::read(conn, &user)?.is_some() {
            let _ = funds::apply(conn, &user, cfg.starting_capital, &now_s, |f| {
                Ok(funds::prior_session_release(f, margin, pnl))
            })?;
        }
        p.quantity = 0;
        p.margin_blocked = Decimal::ZERO;
        p.pnl = pnl;
        p.accumulated_realized_pnl += pnl;
        p.today_realized_pnl = Decimal::ZERO;
        // Keep updated_at: the row belongs to the closed session.
        conn.execute(
            "UPDATE sandbox_positions SET quantity = 0, margin_blocked = '0', pnl = ?1,
               accumulated_realized_pnl = ?2, today_realized_pnl = '0'
             WHERE id = ?3",
            params![
                dec_to_db(p.pnl),
                dec_to_db(p.accumulated_realized_pnl),
                p.id
            ],
        )?;
        tracing::info!(
            "Catch-up settled leftover MIS {} {} at {} (P&L {})",
            p.symbol,
            p.exchange,
            settle,
            pnl
        );
        n += 1;
    }
    Ok(n)
}

/// Zero `today_realized_pnl` and `today_charges` written before the last
/// boundary (web `catch_up_daily_pnl_reset`). A funds row is stale when
/// either is still set: a day of buying only (delivery, or a position
/// carried overnight) pays charges and realizes nothing.
pub(crate) fn reset_stale_daily_pnl(conn: &Connection, core: &Core) -> rusqlite::Result<usize> {
    let boundary = ts(session::last_session_expiry(
        core.now(),
        core.opts.session_expiry,
    ));
    let p = conn.execute(
        "UPDATE sandbox_positions SET today_realized_pnl = '0'
         WHERE today_realized_pnl != '0' AND updated_at < ?1",
        params![boundary],
    )?;
    let f = conn.execute(
        "UPDATE sandbox_funds SET today_realized_pnl = '0', today_charges = '0'
         WHERE (today_realized_pnl != '0' OR today_charges != '0') AND updated_at < ?1",
        params![boundary],
    )?;
    Ok(p + f)
}

/// Backfill yesterday's snapshot when the 23:59 job was missed (web
/// `catch_up_daily_pnl_snapshot`), for a book that trades on that day
/// ([`super::squareoff::snapshot_day`]: crypto books at weekends too).
pub(crate) fn backfill_snapshot(conn: &Connection, core: &Core) -> rusqlite::Result<bool> {
    let now = core.now();
    let yesterday = now.date() - Duration::days(1);
    let d = yesterday.format("%Y-%m-%d").to_string();
    let users: Vec<String> = {
        let mut stmt = conn.prepare_cached("SELECT user_id FROM sandbox_funds ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut any = false;
    for user in users {
        if !super::squareoff::snapshot_day(conn, core, &user, yesterday)? {
            continue;
        }
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM sandbox_daily_pnl WHERE user_id = ?1 AND date = ?2",
                params![user, d],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            continue;
        }
        let Some(f) = funds::read(conn, &user)? else {
            continue;
        };
        let yesterday_realized = f.realized_pnl - f.today_realized_pnl;
        // Charges need no reconstruction: every trade carries what it paid.
        let yesterday_charges = charges_on(conn, &user, &d)?;
        // A day of buying only realizes nothing and still paid charges.
        if yesterday_realized == Decimal::ZERO
            && f.realized_pnl == Decimal::ZERO
            && yesterday_charges == Decimal::ZERO
        {
            continue;
        }
        conn.execute(
            "INSERT INTO sandbox_daily_pnl (user_id, date, realized_pnl, positions_unrealized_pnl,
               holdings_unrealized_pnl, total_mtm, charges, available_balance, used_margin,
               portfolio_value, created_at)
             VALUES (?1, ?2, ?3, '0', '0', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                user,
                d,
                dec_to_db(yesterday_realized),
                dec_to_db(yesterday_charges),
                dec_to_db(f.available_balance),
                dec_to_db(f.used_margin),
                dec_to_db(f.available_balance + f.used_margin),
                ts(now)
            ],
        )?;
        any = true;
    }
    Ok(any)
}

/// What `user`'s trades on the IST day `day` (`YYYY-MM-DD`) paid.
fn charges_on(conn: &Connection, user: &str, day: &str) -> rusqlite::Result<Decimal> {
    let mut stmt = conn.prepare_cached(
        "SELECT total_charges FROM sandbox_trades
         WHERE user_id = ?1 AND substr(trade_timestamp, 1, 10) = ?2",
    )?;
    let rows = stmt.query_map(params![user, day], |r| r.get::<_, String>(0))?;
    let mut sum = Decimal::ZERO;
    for v in rows {
        sum += super::types::dec_from_db(&v?);
    }
    Ok(sum)
}

/// Run every catch-up step. Returns `None` when another run is in progress.
pub(crate) async fn run(core: &Arc<Core>) -> SbResult<Option<CatchUpReport>> {
    let Ok(_one) = core.catch_up_lock.try_lock() else {
        tracing::info!("Sandbox catch-up already running; skipping this trigger");
        return Ok(None);
    };
    let mut report = CatchUpReport::default();

    let (mis, expired, t1, reset, snap) = blocking(core, |c| {
        let mut outbox = Outbox::new();
        let r = c.db.with_tx(|tx| {
            let cfg = SandboxConfig::load(tx)?;
            let reset = reset_stale_daily_pnl(tx, c)?;
            let mis = settle_stale_mis(tx, c, &cfg)?;
            let expired = expiry::cleanup_expired_contracts(tx, c, &cfg, c.now())?;
            let t1 = if holdings::pending_t1(tx, c.now())? > 0 {
                holdings::process_t1_in_tx(tx, c, c.now(), &mut outbox)?
            } else {
                0
            };
            let snap = backfill_snapshot(tx, c)?;
            Ok::<_, SandboxError>((mis, expired, t1, reset, snap))
        })?;
        c.publish(outbox);
        Ok(r)
    })
    .await?;
    report.stale_mis_settled = mis;
    report.expired_settled = expired;
    report.t1_settled = t1;
    report.pnl_rows_reset = reset;
    report.snapshot_backfilled = snap;

    let (reclaimed, expired_gtts) = gtt::maintain(core).await?;
    report.gtt_reclaimed = reclaimed;
    report.gtt_expired = expired_gtts;
    report.gtt_fired = gtt::poll_active(core).await?;
    tracing::info!("Sandbox catch-up done: {:?}", report);
    Ok(Some(report))
}
