//! MIS auto square-off and the sandbox day schedule (web
//! `sandbox/squareoff_manager.py`, `sandbox/squareoff_thread.py`).
//!
//! The sweep: cancel every resting MIS order past its exchange's square-off
//! time, cancel resting orders on expired contracts, settle expired
//! positions, then close every MIS position past its time with a MARKET
//! order tagged `AUTO_SQUARE_OFF`. One sweep at a time; the exchange's own
//! job and the one-minute backup can both fire and the second finds nothing
//! left to do.
//!
//! The schedule (IST): square-off per exchange group at its configured time,
//! the backup sweep every minute, T+1 at 00:00, the daily P&L snapshot at
//! 23:59 (trading days, and every day for a book holding or trading
//! crypto), today's P&L reset at the session boundary (03:00), and the
//! optional weekly fund reset. CRYPTO has no square-off time, so no sweep
//! and no leftover-MIS settlement touches a crypto position.

use super::clock::{ts, IST};
use super::config::SandboxConfig;
use super::core::{blocking, Core};
use super::events::{auto_squareoff, Outbox};
use super::expiry;
use super::funds;
use super::holdings;
use super::orders;
use super::positions;
use super::replies::{JobStatus, SquareOffStatus};
use super::types::{dec_to_db, PriceType, Product, SandboxError, SbResult};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Weekday};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::sync::Arc;

/// What a sweep did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SweepReport {
    pub cancelled_orders: usize,
    pub settled_expired: usize,
    /// Positions whose close filled.
    pub closed_positions: usize,
    /// Positions whose close order rests until a usable price arrives.
    pub pending_closures: usize,
    pub failed_closures: usize,
}

/// Cancels the pending orders that are due: MIS orders past their
/// square-off time and orders on expired contracts, each in its own
/// savepoint. A square-off close still waiting for a price is the
/// square-off itself and is kept, unless its contract expired.
fn cancel_due_orders(
    conn: &Connection,
    c: &Core,
    cfg: &SandboxConfig,
    now: NaiveDateTime,
    outbox: &mut Outbox,
) -> SbResult<usize> {
    let t = now.time();
    let mut cancelled = 0usize;
    for o in orders::pending(conn, None)? {
        let mis_due = o.product == Product::Mis
            && cfg
                .square_off_time(&o.exchange)
                .map(|sq| t >= sq)
                .unwrap_or(false);
        let expired = expiry::is_contract_expired_now(
            expiry::contract_expiry(c, &o.symbol, &o.exchange),
            &o.exchange,
            now,
            cfg,
        );
        if !(mis_due || expired) {
            continue;
        }
        // A square-off close still waiting for a price is the
        // square-off itself: the backup sweep keeps it rather than
        // cancelling it and placing another.
        let own_close =
            o.strategy.as_deref() == Some("AUTO_SQUARE_OFF") && o.price_type == PriceType::Market;
        if own_close && !expired {
            continue;
        }
        conn.execute_batch("SAVEPOINT sq_cancel")?;
        match orders::cancel_in_tx(conn, c, &o, outbox) {
            Ok(()) => {
                conn.execute_batch("RELEASE sq_cancel")?;
                cancelled += 1;
                tracing::info!("Auto-cancelled sandbox order {} ({})", o.orderid, o.symbol);
            }
            Err(e) => {
                conn.execute_batch("ROLLBACK TO sq_cancel; RELEASE sq_cancel")?;
                tracing::warn!(
                    "Could not auto-cancel sandbox order {}: {}",
                    o.orderid,
                    e.message
                );
            }
        }
    }
    Ok(cancelled)
}

/// The MIS positions past their exchange's square-off time.
fn mis_positions_due(
    conn: &Connection,
    c: &Core,
    cfg: &SandboxConfig,
    t: NaiveTime,
) -> SbResult<Vec<(String, String)>> {
    let mut to_close = Vec::new();
    for p in positions::list_open(conn, Some(Product::Mis))? {
        if p.user_id != c.user() {
            continue;
        }
        if let Some(sq) = cfg.square_off_time(&p.exchange) {
            if t >= sq {
                to_close.push((p.symbol.clone(), p.exchange.clone()));
            }
        }
    }
    Ok(to_close)
}

/// Closes the MIS positions past their time, each under its lock. A
/// close already waiting for a price is not placed again by the backup
/// sweep; only a filled close counts as closed. True when an order was
/// placed.
async fn close_due_positions(
    core: &Arc<Core>,
    to_close: Vec<(String, String)>,
    report: &mut SweepReport,
) -> bool {
    let mut placed = false;
    for (symbol, exchange) in to_close {
        match positions::close_position_outcome(core, &symbol, &exchange, "MIS").await {
            Ok(r) => {
                placed |= r.placed;
                match r.outcome {
                    positions::CloseOutcome::Filled => {
                        report.closed_positions += 1;
                        tracing::info!(
                            "Auto square-off of {} {}: order {}",
                            symbol,
                            exchange,
                            r.reply.orderid
                        );
                    }
                    positions::CloseOutcome::Pending => {
                        report.pending_closures += 1;
                        tracing::warn!(
                            "Auto square-off of {} {} is waiting for a price: order {} fills when one arrives",
                            symbol,
                            exchange,
                            r.reply.orderid
                        );
                    }
                    positions::CloseOutcome::Failed => {
                        report.failed_closures += 1;
                        tracing::warn!(
                            "Auto square-off of {} {}: order {} was refused",
                            symbol,
                            exchange,
                            r.reply.orderid
                        );
                    }
                }
            }
            Err(e) if e.http_status == 404 => {}
            Err(e) => {
                report.failed_closures += 1;
                tracing::warn!(
                    "Auto square-off of {} {} failed: {}",
                    symbol,
                    exchange,
                    e.message
                );
            }
        }
    }
    placed
}

/// One square-off sweep.
pub(crate) async fn sweep(core: &Arc<Core>) -> SbResult<SweepReport> {
    let _one = core.sweep_lock.lock().await;
    let mut report = SweepReport::default();

    // Steps 1, 1b, 1c in one transaction.
    let (cancelled, settled, to_close) = blocking(core, |c| {
        let mut outbox = Outbox::new();
        let r = c.db.with_tx(|tx| {
            let cfg = SandboxConfig::load(tx)?;
            let now = c.now();
            let t = now.time();
            let cancelled = cancel_due_orders(tx, c, &cfg, now, &mut outbox)?;
            let settled = expiry::cleanup_expired_contracts(tx, c, &cfg, now)?;
            let to_close = mis_positions_due(tx, c, &cfg, t)?;
            Ok::<_, SandboxError>((cancelled, settled, to_close))
        })?;
        c.publish(outbox);
        Ok(r)
    })
    .await?;
    report.cancelled_orders = cancelled;
    report.settled_expired = settled;

    // Step 2: close MIS positions past their time.
    let placed = close_due_positions(core, to_close, &mut report).await;
    if report.cancelled_orders > 0 || report.closed_positions > 0 || placed {
        let mut outbox = Outbox::new();
        outbox.push(auto_squareoff(
            report.cancelled_orders,
            report.closed_positions,
        ));
        core.publish(outbox);
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Daily jobs
// ---------------------------------------------------------------------------

/// Zero today's realized P&L on every funds row and position (03:00 job).
/// Positions keep their `updated_at`, so yesterday's closed rows stay
/// hidden.
pub(crate) fn reset_daily_pnl(conn: &Connection, now: &str) -> rusqlite::Result<(usize, usize)> {
    let f = conn.execute(
        "UPDATE sandbox_funds SET today_realized_pnl = '0', updated_at = ?1 WHERE today_realized_pnl != '0'",
        params![now],
    )?;
    let p = conn.execute(
        "UPDATE sandbox_positions SET today_realized_pnl = '0' WHERE today_realized_pnl != '0'",
        [],
    )?;
    Ok((f, p))
}

/// Whether the user held a crypto position on `date` (opened by then and
/// still open, or closed that day) or traded crypto that day.
fn crypto_active_on(conn: &Connection, user: &str, date: NaiveDate) -> rusqlite::Result<bool> {
    let day = date.format("%Y-%m-%d").to_string();
    let n: i64 = conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM sandbox_positions WHERE user_id = ?1 AND exchange = 'CRYPTO'
              AND ((quantity != 0 AND substr(created_at, 1, 10) <= ?2)
                   OR substr(updated_at, 1, 10) = ?2))
         + (SELECT COUNT(*) FROM sandbox_trades WHERE user_id = ?1 AND exchange = 'CRYPTO'
              AND substr(trade_timestamp, 1, 10) = ?2)",
        params![user, day],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Whether `user`'s book gets a P&L snapshot for `date`: every day of the
/// configured trading calendar (the Indian market calendar in the app,
/// `market_calendar_service::sandbox_trading_day`), and any other day on
/// which the user held or traded crypto, which trades every day. The web
/// (`is_pnl_snapshot_day`, openalgo cced84316) snapshots every day on a
/// 24/7 crypto install and follows the calendar otherwise; the desktop has
/// no install-wide 24/7 switch, so it asks the book instead, which covers
/// every day with crypto P&L without writing rows for idle ones.
pub(crate) fn snapshot_day(
    conn: &Connection,
    core: &Core,
    user: &str,
    date: NaiveDate,
) -> rusqlite::Result<bool> {
    if (core.opts.trading_day)(date) {
        return Ok(true);
    }
    crypto_active_on(conn, user, date)
}

/// Upsert one day's P&L snapshot for every funds row whose book trades on
/// `date` ([`snapshot_day`]; 23:59 job). Returns how many were written.
pub(crate) fn capture_daily_snapshot(
    conn: &Connection,
    core: &Core,
    date: NaiveDate,
    now: &str,
) -> rusqlite::Result<usize> {
    let users: Vec<String> = {
        let mut stmt = conn.prepare_cached("SELECT user_id FROM sandbox_funds ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let d = date.format("%Y-%m-%d").to_string();
    let mut written = 0;
    for user in &users {
        if !snapshot_day(conn, core, user, date)? {
            continue;
        }
        let Some(f) = funds::read(conn, user)? else {
            continue;
        };
        let pos_unrealized: Decimal = positions::list_user(conn, user)?
            .iter()
            .filter(|p| p.quantity != 0)
            .map(|p| p.pnl)
            .sum();
        let hold_unrealized: Decimal = holdings::list_user(conn, user)?.iter().map(|h| h.pnl).sum();
        let realized = f.today_realized_pnl;
        let total_mtm = realized + pos_unrealized + hold_unrealized;
        let portfolio = f.available_balance + f.used_margin;
        conn.execute(
            "INSERT INTO sandbox_daily_pnl (user_id, date, realized_pnl, positions_unrealized_pnl,
               holdings_unrealized_pnl, total_mtm, available_balance, used_margin, portfolio_value, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(user_id, date) DO UPDATE SET realized_pnl = excluded.realized_pnl,
               positions_unrealized_pnl = excluded.positions_unrealized_pnl,
               holdings_unrealized_pnl = excluded.holdings_unrealized_pnl,
               total_mtm = excluded.total_mtm, available_balance = excluded.available_balance,
               used_margin = excluded.used_margin, portfolio_value = excluded.portfolio_value",
            params![
                user,
                d,
                dec_to_db(realized),
                dec_to_db(pos_unrealized),
                dec_to_db(hold_unrealized),
                dec_to_db(total_mtm),
                dec_to_db(f.available_balance),
                dec_to_db(f.used_margin),
                dec_to_db(portfolio),
                now
            ],
        )?;
        written += 1;
    }
    Ok(written)
}

/// Reset a user's sandbox account to the starting capital: funds back to
/// capital (`reset_count += 1`), positions and holdings deleted, and resting
/// orders and active GTTs cancelled so no margin is left owed to a book that
/// no longer holds it. Orders and trades stay as history.
pub(crate) fn reset_account(
    conn: &Connection,
    user: &str,
    capital: Decimal,
    now: &str,
) -> rusqlite::Result<()> {
    funds::reset_row(conn, user, capital, now)?;
    conn.execute(
        "DELETE FROM sandbox_positions WHERE user_id = ?1",
        params![user],
    )?;
    conn.execute(
        "DELETE FROM sandbox_holdings WHERE user_id = ?1",
        params![user],
    )?;
    conn.execute(
        "UPDATE sandbox_orders SET order_status = 'cancelled', update_timestamp = ?1
         WHERE user_id = ?2 AND order_status IN ('open','trigger pending')",
        params![now, user],
    )?;
    conn.execute(
        "UPDATE sandbox_gtt_legs SET leg_status = 'cancelled', claimed_at = NULL
         WHERE leg_status IN ('pending','triggering')
           AND gtt_id IN (SELECT gtt_id FROM sandbox_gtt WHERE user_id = ?1 AND gtt_status = 'active')",
        params![user],
    )?;
    conn.execute(
        "UPDATE sandbox_gtt SET gtt_status = 'cancelled', margin_blocked = '0', updated_at = ?1
         WHERE user_id = ?2 AND gtt_status = 'active'",
        params![now, user],
    )?;
    Ok(())
}

/// Weekly auto reset of every funds row (web `reset_all_user_funds`).
pub(crate) fn reset_all_funds(
    conn: &Connection,
    capital: Decimal,
    now: &str,
) -> rusqlite::Result<usize> {
    let users: Vec<String> = {
        let mut stmt = conn.prepare_cached("SELECT user_id FROM sandbox_funds ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for u in &users {
        reset_account(conn, u, capital, now)?;
    }
    Ok(users.len())
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// A scheduled job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    SquareOff,
    BackupSweep,
    T1Settlement,
    DailySnapshot,
    DailyPnlReset,
    AutoReset,
}

#[derive(Debug, Clone, PartialEq)]
enum Recur {
    Daily(NaiveTime),
    Weekly(Weekday, NaiveTime),
    Every(Duration),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub kind: JobKind,
    recur: Recur,
    pub next: NaiveDateTime,
}

/// Late jobs within this grace run; later than this, only idempotent state
/// sweeps still run (the snapshot is skipped and backfilled by catch-up).
pub const MISFIRE_GRACE_SECS: i64 = 300;

fn next_after(recur: &Recur, after: NaiveDateTime) -> NaiveDateTime {
    match recur {
        Recur::Daily(t) => {
            let today = after.date().and_time(*t);
            if today > after {
                today
            } else {
                today + Duration::days(1)
            }
        }
        Recur::Weekly(day, t) => {
            let mut d = after.date();
            for _ in 0..8 {
                if d.weekday() == *day && d.and_time(*t) > after {
                    return d.and_time(*t);
                }
                d += Duration::days(1);
            }
            after + Duration::days(7)
        }
        Recur::Every(step) => after + *step,
    }
}

/// The sandbox day schedule, driven by the injected clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheduler {
    pub jobs: Vec<Job>,
}

impl Scheduler {
    /// Build every job from config, first runs strictly after `now`.
    pub fn build(cfg: &SandboxConfig, session_expiry: NaiveTime, now: NaiveDateTime) -> Self {
        let mut jobs = Vec::new();
        let mut add = |id: &str, name: String, kind: JobKind, recur: Recur| {
            let next = next_after(&recur, now);
            jobs.push(Job {
                id: id.to_string(),
                name,
                kind,
                recur,
                next,
            });
        };
        for (group, t) in [
            ("NSE_BSE", cfg.nse_bse_square_off_time),
            ("CDS_BCD", cfg.cds_bcd_square_off_time),
            ("MCX", cfg.mcx_square_off_time),
            ("NCDEX", cfg.ncdex_square_off_time),
        ] {
            add(
                &format!("squareoff_{group}"),
                format!("MIS Square-off {group}"),
                JobKind::SquareOff,
                Recur::Daily(t),
            );
        }
        add(
            "squareoff_backup",
            "MIS Square-off Backup Check".into(),
            JobKind::BackupSweep,
            Recur::Every(Duration::seconds(60)),
        );
        add(
            "t1_settlement",
            "T+1 Settlement (CNC to Holdings)".into(),
            JobKind::T1Settlement,
            Recur::Daily(NaiveTime::MIN),
        );
        if let Some(day) = cfg.reset_day {
            add(
                "auto_reset",
                format!(
                    "Auto-Reset Funds ({} {})",
                    weekday_name(day),
                    cfg.reset_time.format("%H:%M")
                ),
                JobKind::AutoReset,
                Recur::Weekly(day, cfg.reset_time),
            );
        }
        add(
            "daily_pnl_snapshot",
            "Daily PnL Snapshot (23:59 IST)".into(),
            JobKind::DailySnapshot,
            Recur::Daily(NaiveTime::from_hms_opt(23, 59, 0).unwrap_or(NaiveTime::MIN)),
        );
        add(
            "daily_pnl_reset",
            format!("Daily PnL Reset ({} IST)", session_expiry.format("%H:%M")),
            JobKind::DailyPnlReset,
            Recur::Daily(session_expiry),
        );
        Self { jobs }
    }

    /// Jobs due at `now`, in schedule order, each advanced to its next
    /// occurrence after `now` (a job late by more than the misfire grace is
    /// coalesced into one run; the snapshot is skipped instead).
    pub fn due(&mut self, now: NaiveDateTime) -> Vec<(String, JobKind)> {
        let mut due: Vec<(NaiveDateTime, String, JobKind)> = Vec::new();
        for job in &mut self.jobs {
            if job.next > now {
                continue;
            }
            let late = (now - job.next).num_seconds() > MISFIRE_GRACE_SECS;
            if !(late && job.kind == JobKind::DailySnapshot) {
                due.push((job.next, job.id.clone(), job.kind));
            }
            job.next = next_after(&job.recur, now);
        }
        due.sort_by(|a, b| a.0.cmp(&b.0));
        due.into_iter().map(|(_, id, k)| (id, k)).collect()
    }

    /// Status rows (`next_run` as `YYYY-MM-DD HH:MM:SS IST`).
    pub fn status(&self) -> Vec<JobStatus> {
        self.jobs
            .iter()
            .map(|j| JobStatus {
                id: j.id.clone(),
                name: j.name.clone(),
                next_run: match IST.from_local_datetime(&j.next) {
                    chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
                    _ => j.next.format("%Y-%m-%d %H:%M:%S").to_string(),
                },
            })
            .collect()
    }

    pub fn status_reply(&self, running: bool) -> SquareOffStatus {
        if !running {
            return SquareOffStatus {
                running: false,
                timezone: None,
                jobs: vec![],
            };
        }
        SquareOffStatus {
            running: true,
            timezone: Some("Asia/Kolkata".to_string()),
            jobs: self.status(),
        }
    }
}

fn weekday_name(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// Run one scheduled job.
pub(crate) async fn run_job(core: &Arc<Core>, kind: JobKind) -> SbResult<()> {
    match kind {
        JobKind::SquareOff | JobKind::BackupSweep => {
            sweep(core).await?;
        }
        JobKind::T1Settlement => {
            holdings::process_t1(core).await?;
        }
        JobKind::DailySnapshot => {
            blocking(core, |c| {
                let now = c.now();
                let written =
                    c.db.with_tx(|tx| capture_daily_snapshot(tx, c, now.date(), &ts(now)))?;
                if written == 0 {
                    tracing::info!(
                        "No sandbox P&L snapshot for {}: not a trading day and no crypto held or traded",
                        now.date()
                    );
                }
                Ok(())
            })
            .await?;
        }
        JobKind::DailyPnlReset => {
            blocking(core, |c| {
                let now = c.now_ts();
                c.db.with_tx(|tx| reset_daily_pnl(tx, &now))?;
                Ok(())
            })
            .await?;
        }
        JobKind::AutoReset => {
            blocking(core, |c| {
                let now = c.now_ts();
                c.db.with_tx(|tx| {
                    let cfg = SandboxConfig::load(tx)?;
                    reset_all_funds(tx, cfg.starting_capital, &now)
                })?;
                tracing::info!("Sandbox funds auto-reset to the starting capital");
                Ok(())
            })
            .await?;
        }
    }
    Ok(())
}
