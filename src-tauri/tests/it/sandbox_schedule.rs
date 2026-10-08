//! Time-driven behaviour on an injected clock: the MIS time gate, the
//! square-off sweep and its schedule, T+1 at 00:00, the 23:59 snapshot, the
//! 03:00 boundary, the weekly reset, expiry settlement, and catch-up across
//! simulated days (web `squareoff_manager.py`, `squareoff_thread.py`,
//! `holdings_manager.py`, `catch_up_processor.py`,
//! `test/sandbox/test_catch_up_session_boundary.py`).
//!
//! 2026-10-05 is a Monday.

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::SandboxOptions;
use rusqlite::params;
use rust_decimal::Decimal;
use sandbox_support::*;

#[allow(clippy::too_many_arguments)]
fn seed_position(
    env: &Env,
    symbol: &str,
    exchange: &str,
    product: &str,
    qty: i64,
    avg: &str,
    margin: &str,
    created: &str,
    updated: &str,
) {
    env.sb
        .db()
        .with_tx(|tx| -> rusqlite::Result<()> {
            tx.execute(
                "INSERT INTO sandbox_positions (user_id, symbol, exchange, product, quantity, average_price,
                   ltp, pnl, pnl_percent, accumulated_realized_pnl, today_realized_pnl, margin_blocked,
                   created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, '0', '0', '0', '0', ?7, ?8, ?9)",
                params![USER, symbol, exchange, product, qty, avg, margin, created, updated],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO sandbox_funds (user_id, total_capital, available_balance, used_margin,
                   realized_pnl, today_realized_pnl, unrealized_pnl, total_pnl, last_reset_date, reset_count,
                   created_at, updated_at)
                 VALUES (?1, '10000000', '10000000', '0', '0', '0', '0', '0', ?2, 0, ?2, ?2)",
                params![USER, created],
            )?;
            let m: rust_decimal::Decimal = d(margin);
            if m > Decimal::ZERO {
                let f: (String, String) = tx.query_row(
                    "SELECT available_balance, used_margin FROM sandbox_funds WHERE user_id = ?1",
                    params![USER],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                tx.execute(
                    "UPDATE sandbox_funds SET available_balance = ?1, used_margin = ?2 WHERE user_id = ?3",
                    params![
                        (d(&f.0) - m).normalize().to_string(),
                        (d(&f.1) + m).normalize().to_string(),
                        USER
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

// ---------------------------------------------------------------------------
// MIS time gate (order_manager.py L150-210)
// ---------------------------------------------------------------------------

async fn mis_allowed(
    env: &Env,
    symbol: &str,
    exchange: &str,
    qty: i64,
    at: &str,
) -> Result<(), String> {
    env.set_time(at);
    env.sb
        .place_order(req(symbol, exchange, "BUY", qty, "MARKET", "MIS"))
        .await
        .map(|_| ())
        .map_err(|e| e.message)
}

#[tokio::test]
async fn mis_orders_are_refused_after_the_exchange_square_off_time() {
    let env = Env::at("2026-10-05 09:00:00");
    for (s, e) in [
        ("SBIN", "NSE"),
        ("NIFTY27OCT26FUT", "NFO"),
        ("USDINR29OCT26FUT", "CDS"),
        ("CRUDEOIL19OCT26FUT", "MCX"),
        ("GUARSEED10NOV26FUT", "NCDEX"),
        ("BTCUSD.P", "CRYPTO"),
    ] {
        env.ltp(s, e, "100");
    }
    assert!(mis_allowed(&env, "SBIN", "NSE", 1, "2026-10-05 09:00:00")
        .await
        .is_ok());
    assert!(mis_allowed(&env, "SBIN", "NSE", 1, "2026-10-05 15:14:59")
        .await
        .is_ok());
    let msg = mis_allowed(&env, "SBIN", "NSE", 1, "2026-10-05 15:15:00")
        .await
        .unwrap_err();
    assert_eq!(
        msg,
        "MIS orders cannot be placed after square-off time (15:15 IST). Trading resumes at 09:00 AM IST."
    );
    assert!(mis_allowed(&env, "SBIN", "NSE", 1, "2026-10-06 08:59:59")
        .await
        .is_err());
    // NFO follows the NSE/BSE time.
    assert!(
        mis_allowed(&env, "NIFTY27OCT26FUT", "NFO", 65, "2026-10-05 15:16:00")
            .await
            .is_err()
    );
    assert!(
        mis_allowed(&env, "USDINR29OCT26FUT", "CDS", 1, "2026-10-05 16:44:00")
            .await
            .is_ok()
    );
    assert!(
        mis_allowed(&env, "USDINR29OCT26FUT", "CDS", 1, "2026-10-05 16:45:00")
            .await
            .is_err()
    );
    assert!(mis_allowed(
        &env,
        "CRUDEOIL19OCT26FUT",
        "MCX",
        100,
        "2026-10-05 23:29:00"
    )
    .await
    .is_ok());
    assert!(mis_allowed(
        &env,
        "CRUDEOIL19OCT26FUT",
        "MCX",
        100,
        "2026-10-05 23:30:00"
    )
    .await
    .is_err());
    assert!(mis_allowed(
        &env,
        "GUARSEED10NOV26FUT",
        "NCDEX",
        5,
        "2026-10-05 16:59:00"
    )
    .await
    .is_ok());
    assert!(mis_allowed(
        &env,
        "GUARSEED10NOV26FUT",
        "NCDEX",
        5,
        "2026-10-05 17:00:00"
    )
    .await
    .is_err());
    // No square-off time for crypto: never gated.
    assert!(
        mis_allowed(&env, "BTCUSD.P", "CRYPTO", 1, "2026-10-06 02:00:00")
            .await
            .is_ok()
    );
    env.shutdown().await;
}

#[tokio::test]
async fn a_reducing_mis_order_is_allowed_after_square_off_time() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.set_time("2026-10-05 15:20:00");
    assert!(env
        .sb
        .place_order(req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS"))
        .await
        .is_err());
    let id = place(&env, req("SBIN", "NSE", "SELL", 4, "MARKET", "MIS")).await;
    assert_eq!(env.status(&id).await, "complete");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 6);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Square-off sweep and schedule
// ---------------------------------------------------------------------------

#[tokio::test]
async fn square_off_at_1515_cancels_resting_mis_orders_then_closes_mis_positions() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    env.ltp("CRUDEOIL19OCT26FUT", "MCX", "6000");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    let resting = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 5, "LIMIT", "MIS"), "90"),
    )
    .await;
    let cnc = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 5, "LIMIT", "CNC"), "90"),
    )
    .await;
    place(
        &env,
        req("CRUDEOIL19OCT26FUT", "MCX", "BUY", 100, "MARKET", "MIS"),
    )
    .await;
    assert!(
        env.sb.run_due_jobs().await.unwrap().is_empty(),
        "the first call builds the schedule"
    );

    env.set_time("2026-10-05 15:15:20");
    env.ltp("SBIN", "NSE", "102");
    let ran = env.sb.run_due_jobs().await.unwrap();
    assert_eq!(ran.iter().filter(|j| *j == "squareoff_NSE_BSE").count(), 1);
    assert!(ran.contains(&"squareoff_backup".to_string()));
    assert_eq!(env.status(&resting).await, "cancelled");
    assert_eq!(
        env.status(&cnc).await,
        "open",
        "CNC orders are not squared off"
    );
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    assert_eq!(
        env.qty("CRUDEOIL19OCT26FUT", "MCX", "MIS").await,
        100,
        "MCX waits for 23:30"
    );
    let ob = env.sb.orderbook().await.unwrap();
    let auto: Vec<_> = ob
        .data
        .orders
        .iter()
        .filter(|o| o.strategy == "AUTO_SQUARE_OFF")
        .collect();
    assert_eq!(auto.len(), 1);
    assert_eq!(auto[0].action, "SELL");
    assert_eq!(auto[0].quantity, 10);
    assert_eq!(auto[0].order_status, "complete");
    env.rec.wait_for("sandbox.auto_squareoff", 1).await;

    // Running again at the same minute does nothing more.
    assert!(env.sb.run_due_jobs().await.unwrap().is_empty());
    env.sb.square_off_now().await.unwrap();
    assert_eq!(
        env.sb
            .orderbook()
            .await
            .unwrap()
            .data
            .orders
            .iter()
            .filter(|o| o.strategy == "AUTO_SQUARE_OFF")
            .count(),
        1,
        "the primary and backup sweeps close a position once"
    );

    env.set_time("2026-10-05 23:30:05");
    env.sb.run_due_jobs().await.unwrap();
    assert_eq!(env.qty("CRUDEOIL19OCT26FUT", "MCX", "MIS").await, 0);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn the_schedule_fires_each_job_once_and_coalesces_after_a_long_gap() {
    let env = Env::at("2026-10-05 10:00:00");
    env.sb.run_due_jobs().await.unwrap();
    env.set_time("2026-10-05 15:15:00");
    let ran = env.sb.run_due_jobs().await.unwrap();
    assert_eq!(
        ran,
        vec![
            "squareoff_backup".to_string(),
            "squareoff_NSE_BSE".to_string()
        ]
    );
    // A whole day later (the machine slept): each daily job once, in
    // schedule order; the late 23:59 snapshot is skipped (catch-up backfills).
    env.set_time("2026-10-06 15:16:00");
    let ran = env.sb.run_due_jobs().await.unwrap();
    for id in [
        "squareoff_CDS_BCD",
        "squareoff_NCDEX",
        "squareoff_MCX",
        "t1_settlement",
        "daily_pnl_reset",
        "squareoff_NSE_BSE",
    ] {
        assert_eq!(
            ran.iter().filter(|j| *j == id).count(),
            1,
            "{id} in {ran:?}"
        );
    }
    assert!(!ran.contains(&"daily_pnl_snapshot".to_string()));
    let status = env.sb.squareoff_status().await;
    assert!(!status.data.running);
    env.shutdown().await;
}

#[tokio::test]
async fn squareoff_status_lists_the_web_jobs() {
    let env = Env::at("2026-10-05 10:00:00");
    env.sb.update_config("reset_day", "Sunday").await.unwrap();
    let idle = env.sb.squareoff_status().await;
    assert!(!idle.data.running);
    assert!(
        idle.data.jobs.is_empty(),
        "the web lists no jobs while stopped"
    );
    env.sb
        .start_engine(std::sync::Arc::new(
            openalgo_desktop_lib::sandbox::BroadcastTicks::new(16),
        ))
        .await
        .unwrap();
    let s = env.sb.reload_squareoff().await;
    assert!(s.data.running);
    let ids: Vec<&str> = s.data.jobs.iter().map(|j| j.id.as_str()).collect();
    for id in [
        "squareoff_NSE_BSE",
        "squareoff_CDS_BCD",
        "squareoff_MCX",
        "squareoff_NCDEX",
        "squareoff_backup",
        "t1_settlement",
        "auto_reset",
        "daily_pnl_snapshot",
        "daily_pnl_reset",
    ] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
    let nse = s
        .data
        .jobs
        .iter()
        .find(|j| j.id == "squareoff_NSE_BSE")
        .unwrap();
    assert_eq!(nse.next_run, "2026-10-05 15:15:00 IST");
    env.shutdown().await;
}

#[tokio::test]
async fn changing_a_square_off_time_moves_the_job() {
    let env = Env::at("2026-10-05 10:00:00");
    env.sb.run_due_jobs().await.unwrap();
    env.sb
        .update_config("nse_bse_square_off_time", "15:00")
        .await
        .unwrap();
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    env.sb.run_due_jobs().await.unwrap();
    env.set_time("2026-10-05 15:00:30");
    let ran = env.sb.run_due_jobs().await.unwrap();
    assert!(ran.contains(&"squareoff_NSE_BSE".to_string()), "{ran:?}");
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// T+1, snapshot, 03:00 boundary, weekly reset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t1_at_midnight_moves_yesterdays_cnc_and_leaves_todays() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "800");
    env.ltp("INFY", "NSE", "1500");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "CNC")).await;
    assert_eq!(env.used().await, d("8000"));
    env.sb.run_due_jobs().await.unwrap();
    env.set_time("2026-10-06 00:00:10");
    let ran = env.sb.run_due_jobs().await.unwrap();
    assert!(ran.contains(&"t1_settlement".to_string()));
    let h = env.sb.holdings().await.unwrap();
    assert_eq!(h.data.holdings.len(), 1);
    let row = &h.data.holdings[0];
    assert_eq!(
        (row.symbol.as_str(), row.quantity, row.average_price),
        ("SBIN", 10, 800.0)
    );
    assert_eq!(row.settlement_date, "2026-10-06");
    assert_eq!(env.used().await, d("0"), "the margin became the holding");
    assert_eq!(
        env.available().await,
        d("9992000"),
        "no cash comes back on a buy"
    );
    assert!(env
        .sb
        .position_row("SBIN", "NSE", "CNC")
        .await
        .unwrap()
        .is_none());
    env.rec.wait_for("sandbox.t1_settlement", 1).await;

    env.set_time("2026-10-06 01:00:00");
    place(&env, req("INFY", "NSE", "BUY", 2, "MARKET", "CNC")).await;
    env.set_time("2026-10-06 02:00:00");
    assert_eq!(
        env.sb.t1_settlement().await.unwrap(),
        0,
        "a position from today waits for tomorrow"
    );
    assert_eq!(env.qty("INFY", "NSE", "CNC").await, 2);
    env.shutdown().await;
}

// Port of test_catch_up_session_boundary.py::test_t1_settlement_does_not_sweep_in_a_position_created_early_today
#[tokio::test]
async fn test_t1_settlement_does_not_sweep_in_a_position_created_early_today() {
    let env = Env::at("2026-10-06 06:00:00");
    seed_position(
        &env,
        "SBIN",
        "NSE",
        "CNC",
        10,
        "800",
        "8000",
        "2026-10-06 01:00:00.000000",
        "2026-10-06 01:00:00.000000",
    );
    let r = env.sb.catch_up().await.unwrap().unwrap();
    assert_eq!(r.t1_settled, 0);
    assert_eq!(env.qty("SBIN", "NSE", "CNC").await, 10);
    env.shutdown().await;
}

#[tokio::test]
async fn todays_realized_pnl_resets_at_the_0300_boundary_and_all_time_is_kept() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.ltp("SBIN", "NSE", "110");
    place(&env, req("SBIN", "NSE", "SELL", 10, "MARKET", "MIS")).await;
    env.sb.run_due_jobs().await.unwrap();
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.today_realized_pnl, 100.0);
    env.set_time("2026-10-06 02:59:00");
    env.sb.run_due_jobs().await.unwrap();
    assert_eq!(env.sb.funds().await.unwrap().data.today_realized_pnl, 100.0);
    env.set_time("2026-10-06 03:00:05");
    let ran = env.sb.run_due_jobs().await.unwrap();
    assert!(ran.contains(&"daily_pnl_reset".to_string()));
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.today_realized_pnl, 0.0);
    assert_eq!(f.data.m2mrealized, 0.0);
    assert_eq!(f.data.total_realized_pnl, 100.0);
    let p = env
        .sb
        .position_row("SBIN", "NSE", "MIS")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.today_realized_pnl, Decimal::ZERO);
    assert_eq!(p.accumulated_realized_pnl, d("100"));
    env.shutdown().await;
}

#[tokio::test]
async fn the_snapshot_runs_at_2359_on_trading_days_only() {
    let env = Env::at("2026-10-09 10:00:00"); // Friday
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.ltp("SBIN", "NSE", "105");
    place(&env, req("SBIN", "NSE", "SELL", 10, "MARKET", "MIS")).await;
    env.sb.run_due_jobs().await.unwrap();
    env.set_time("2026-10-09 23:59:10");
    env.sb.run_due_jobs().await.unwrap();
    let p = env.sb.mypnl().await.unwrap();
    assert_eq!(p.data.daily_pnl.len(), 1);
    assert_eq!(p.data.daily_pnl[0].date, "2026-10-09");
    assert_eq!(p.data.daily_pnl[0].realized_pnl, 50.0);
    assert_eq!(p.data.daily_pnl[0].portfolio_value, 10_000_050.0);
    // Saturday 23:59: skipped.
    env.set_time("2026-10-10 23:59:10");
    env.sb.run_due_jobs().await.unwrap();
    assert_eq!(env.sb.mypnl().await.unwrap().data.daily_pnl.len(), 1);
    env.shutdown().await;
}

#[tokio::test]
async fn the_weekly_reset_runs_on_the_configured_day() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "CNC")).await;
    let resting = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "CNC"), "90"),
    )
    .await;
    env.sb.update_config("reset_day", "Tuesday").await.unwrap();
    env.sb.update_config("reset_time", "09:15").await.unwrap();
    env.sb.run_due_jobs().await.unwrap();
    env.set_time("2026-10-06 09:15:30");
    let ran = env.sb.run_due_jobs().await.unwrap();
    assert!(ran.contains(&"auto_reset".to_string()), "{ran:?}");
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.reset_count, 1);
    assert_eq!(f.data.availablecash, 10_000_000.0);
    assert_eq!(f.data.utiliseddebits, 0.0);
    assert!(env.sb.holdings().await.unwrap().data.holdings.is_empty());
    assert_eq!(
        env.status(&resting).await,
        "cancelled",
        "no resting order keeps margin the reset returned"
    );
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn funds_view_applies_a_missed_weekly_reset() {
    let env = Env::at("2026-10-05 10:00:00");
    env.sb.funds().await.unwrap();
    env.sb
        .update_config("reset_day", "Wednesday")
        .await
        .unwrap();
    env.set_time("2026-10-07 00:05:00");
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.reset_count, 1);
    assert_eq!(f.data.last_reset, "2026-10-07 00:05:00");
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.reset_count, 1, "once per reset day");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Expiry settlement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_expiring_option_settles_at_the_nfo_close_at_ltp() {
    let env = Env::at("2026-10-06 10:00:00");
    env.ltp("NIFTY06OCT2622400CE", "NFO", "150");
    place(
        &env,
        req("NIFTY06OCT2622400CE", "NFO", "BUY", 65, "MARKET", "NRML"),
    )
    .await;
    env.ltp("NIFTY06OCT2622400CE", "NFO", "180");
    env.sb.positionbook().await.unwrap();
    env.set_time("2026-10-06 15:39:00");
    env.sb.square_off_now().await.unwrap();
    assert_eq!(env.qty("NIFTY06OCT2622400CE", "NFO", "NRML").await, 65);
    env.set_time("2026-10-06 15:40:00");
    let r = env.sb.square_off_now().await.unwrap();
    assert_eq!(r.settled_expired, 1);
    let p = env
        .sb
        .position_row("NIFTY06OCT2622400CE", "NFO", "NRML")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.quantity, 0);
    assert_eq!(p.accumulated_realized_pnl, d("1950"));
    assert_eq!(env.used().await, d("0"));
    assert_eq!(env.available().await, d("10001950"));
    env.shutdown().await;
}

#[tokio::test]
async fn an_option_settles_at_zero_when_configured_and_not_on_expiry_day_under_next_day() {
    let env = Env::at("2026-10-06 10:00:00");
    env.ltp("NIFTY06OCT2622400CE", "NFO", "150");
    place(
        &env,
        req("NIFTY06OCT2622400CE", "NFO", "BUY", 65, "MARKET", "NRML"),
    )
    .await;
    let resting = place(
        &env,
        limit(
            req("NIFTY06OCT2622400CE", "NFO", "BUY", 65, "LIMIT", "NRML"),
            "100",
        ),
    )
    .await;
    env.sb
        .update_config("expiry_settlement_timing", "next_day")
        .await
        .unwrap();
    env.sb
        .update_config("option_expiry_settlement", "zero")
        .await
        .unwrap();
    env.set_time("2026-10-06 23:00:00");
    env.sb.square_off_now().await.unwrap();
    assert_eq!(env.qty("NIFTY06OCT2622400CE", "NFO", "NRML").await, 65);
    env.set_time("2026-10-07 09:00:00");
    env.sb.square_off_now().await.unwrap();
    let p = env
        .sb
        .position_row("NIFTY06OCT2622400CE", "NFO", "NRML")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.quantity, 0);
    assert_eq!(p.accumulated_realized_pnl, d("-9750"));
    assert_eq!(
        env.status(&resting).await,
        "cancelled",
        "orders on an expired contract are cancelled"
    );
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Catch-up (catch_up_processor.py, test_catch_up_session_boundary.py)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_overnight_mis_position_is_settled_on_catch_up() {
    let env = Env::at("2026-10-06 10:00:00");
    seed_position(
        &env,
        "RELIANCE",
        "NSE",
        "MIS",
        -25,
        "200",
        "1000",
        "2026-10-04 10:00:00.000000",
        "2026-10-05 21:00:00.000000",
    );
    let r = env.sb.catch_up().await.unwrap().unwrap();
    assert_eq!(r.stale_mis_settled, 1);
    let p = env
        .sb
        .position_row("RELIANCE", "NSE", "MIS")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.quantity, 0);
    assert_eq!(
        p.today_realized_pnl,
        Decimal::ZERO,
        "P&L of a closed session is not today's"
    );
    assert_eq!(env.used().await, d("0"));
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.today_realized_pnl, Decimal::ZERO);
    env.shutdown().await;
}

#[tokio::test]
async fn test_reopened_mis_position_survives_catch_up() {
    let env = Env::at("2026-10-06 10:00:00");
    seed_position(
        &env,
        "ZEEL",
        "NSE",
        "MIS",
        50,
        "100",
        "0",
        "2026-10-04 10:00:00.000000",
        "2026-10-06 09:00:00.000000",
    );
    let r = env.sb.catch_up().await.unwrap().unwrap();
    assert_eq!(r.stale_mis_settled, 0);
    assert_eq!(env.qty("ZEEL", "NSE", "MIS").await, 50);
    env.shutdown().await;
}

#[tokio::test]
async fn test_crypto_mis_positions_are_skipped() {
    let opts = SandboxOptions {
        session_expiry_disabled: true,
        ..SandboxOptions::default()
    };
    let env = Env::with_opts("2026-10-06 10:00:00", opts);
    seed_position(
        &env,
        "BTCUSD.P",
        "CRYPTO",
        "MIS",
        10,
        "3000",
        "0",
        "2026-10-04 10:00:00.000000",
        "2026-10-05 21:00:00.000000",
    );
    env.sb.catch_up().await.unwrap();
    assert_eq!(env.qty("BTCUSD.P", "CRYPTO", "MIS").await, 10);
    env.shutdown().await;
}

#[tokio::test]
async fn crypto_mis_positions_trade_24x7_through_sweeps_and_catch_up() {
    // Crypto never closes: no MIS gate, no sweep and no session-boundary
    // settlement, at any hour or on a weekend, even with the session expiry
    // enabled for the Indian exchanges.
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("BTCUSD.P", "CRYPTO", "60000");
    env.ltp("SBIN", "NSE", "100");
    // Saturday, after midnight.
    assert!(
        mis_allowed(&env, "BTCUSD.P", "CRYPTO", 2, "2026-10-10 02:00:00")
            .await
            .is_ok()
    );
    for at in [
        "2026-10-10 15:30:00",
        "2026-10-10 23:59:00",
        "2026-10-11 03:00:00",
    ] {
        env.set_time(at);
        env.sb.square_off_now().await.unwrap();
        assert_eq!(env.qty("BTCUSD.P", "CRYPTO", "MIS").await, 2, "{}", at);
    }
    // Monday, after the 03:00 boundary: catch-up leaves crypto alone.
    env.set_time("2026-10-12 10:00:00");
    env.sb.catch_up().await.unwrap();
    assert_eq!(env.qty("BTCUSD.P", "CRYPTO", "MIS").await, 2);
    env.shutdown().await;
}

#[tokio::test]
async fn test_unconfigured_exchange_mis_positions_are_skipped() {
    let env = Env::at("2026-10-06 10:00:00");
    seed_position(
        &env,
        "UNKNOWN",
        "UNKNOWN",
        "MIS",
        10,
        "100",
        "0",
        "2026-10-04 10:00:00.000000",
        "2026-10-05 21:00:00.000000",
    );
    env.sb.catch_up().await.unwrap();
    assert_eq!(env.qty("UNKNOWN", "UNKNOWN", "MIS").await, 10);
    env.shutdown().await;
}

#[tokio::test]
async fn catch_up_after_the_app_was_closed_for_two_days_runs_what_was_missed_in_order() {
    // Monday: an MIS position left open (the app closed at 14:00), a CNC buy,
    // and a realized gain.
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    env.ltp("INFY", "NSE", "1500");
    env.ltp("TCS", "NSE", "3000");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    place(&env, req("INFY", "NSE", "BUY", 2, "MARKET", "CNC")).await;
    place(&env, req("TCS", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    env.ltp("TCS", "NSE", "3010");
    place(&env, req("TCS", "NSE", "SELL", 1, "MARKET", "MIS")).await;
    env.ltp("SBIN", "NSE", "104");
    env.sb.positionbook().await.unwrap();
    let before = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(before.today_realized_pnl, d("10"));

    // Wednesday 09:00: the app opens again.
    env.set_time("2026-10-07 09:00:00");
    let r = env.sb.catch_up().await.unwrap().unwrap();
    assert_eq!(r.stale_mis_settled, 1, "SBIN MIS settled at its last price");
    assert_eq!(r.t1_settled, 1, "INFY moved to holdings");
    assert!(
        r.pnl_rows_reset >= 1,
        "Monday's realized P&L is no longer today's"
    );
    assert!(r.snapshot_backfilled, "Tuesday's snapshot is backfilled");

    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.today_realized_pnl, Decimal::ZERO);
    assert_eq!(
        f.realized_pnl,
        d("50"),
        "10 from TCS plus 40 from the leftover SBIN"
    );
    assert_eq!(f.used_margin, Decimal::ZERO);
    assert_eq!(
        env.sb.holdings().await.unwrap().data.holdings[0].symbol,
        "INFY"
    );
    let daily = env.sb.mypnl().await.unwrap().data.daily_pnl;
    assert_eq!(daily[0].date, "2026-10-06");
    assert_eq!(daily[0].realized_pnl, 50.0);

    // A second run finds nothing left to do.
    let again = env.sb.catch_up().await.unwrap().unwrap();
    assert_eq!((again.stale_mis_settled, again.t1_settled), (0, 0));
    assert!(!again.snapshot_backfilled);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}
