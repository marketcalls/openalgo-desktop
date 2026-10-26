//! Ports of two web sandbox fixes the desktop already had, proving it.
//!
//! - 103df9fbb, `test/test_gthread_sandbox_order_ids.py`: every sandbox
//!   order gets an order id nobody else has. The web's eight digits after
//!   the date were the clock's microsecond and two random digits, so two
//!   orders in one microsecond clashed one time in a hundred. The desktop
//!   keeps the web's format and retries a clash (`orders::insert_order`).
//! - 0e18e8a7b, `test/test_gthread_sandbox_place_margin.py`: an order that
//!   fails to place leaves no margin blocked. The web committed the margin
//!   before writing the order row; the desktop blocks it and writes the
//!   order in one `BEGIN IMMEDIATE` transaction (`placement::record_order`),
//!   so both land or neither does.

use crate::sandbox_support::*;
use rusqlite::params;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Barrier;

/// The web tests' frozen clock: 2026-10-12 14:00:00.000000 IST, a Monday.
const FROZEN: &str = "2026-10-12 14:00:00";
const CAPITAL: &str = "10000000";

/// Every sandbox order of the test user: (orderid, status, margin held).
fn orders(env: &Env) -> Vec<(String, String, Decimal)> {
    env.sb
        .db()
        .with_conn(|c| -> rusqlite::Result<Vec<(String, String, Decimal)>> {
            let mut stmt = c.prepare(
                "SELECT orderid, order_status, margin_blocked FROM sandbox_orders
                 WHERE user_id = ?1 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![USER], |r| {
                Ok((r.get(0)?, r.get(1)?, d(&r.get::<_, String>(2)?)))
            })?;
            rows.collect()
        })
        .unwrap()
}

/// An order row written directly, as the web tests' `_store_order`.
fn store_order(env: &Env, orderid: &str) {
    env.sb
        .db()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO sandbox_orders (orderid, user_id, symbol, exchange, action, quantity,
                   price_type, product, order_status, pending_quantity, order_timestamp,
                   update_timestamp)
                 VALUES (?1, ?2, 'RELIANCE', 'NSE', 'BUY', 1, 'MARKET', 'CNC', 'complete', 0,
                   '2026-10-12 09:15:00', '2026-10-12 09:15:00')",
                params![orderid, USER],
            )
        })
        .unwrap();
}

/// Make every order insert fail with `message`, on the sandbox's one
/// connection, after the margin has been worked out and blocked.
fn refuse_inserts(env: &Env, message: &str) {
    env.sb
        .db()
        .with_conn(|c| {
            c.execute_batch(&format!(
                "CREATE TEMP TRIGGER refuse_new_order BEFORE INSERT ON sandbox_orders
                 BEGIN SELECT RAISE(ABORT, '{message}'); END;"
            ))
        })
        .unwrap();
}

/// A fresh account whose funds row exists at the starting capital.
async fn account() -> Env {
    let env = Env::at(FROZEN);
    env.sb.funds().await.unwrap();
    assert_untouched(&env).await;
    env
}

async fn assert_untouched(env: &Env) {
    assert_eq!(env.available().await, d(CAPITAL), "available");
    assert_eq!(env.used().await, Decimal::ZERO, "used");
}

/// A BUY LIMIT below the market, so it rests and its margin stays on the
/// order rather than moving to a position.
fn resting_buy(symbol: &str, qty: i64, price: &str) -> openalgo_desktop_lib::sandbox::OrderRequest {
    limit(req(symbol, "NSE", "BUY", qty, "LIMIT", "CNC"), price)
}

// ------------------------------------------------------------ order ids

/// Web `test_ids_stay_unique_when_the_clock_does_not_move`: more orders
/// than the web's old scheme had values for in one microsecond (100),
/// released together. With the clock frozen, every first try draws from
/// those 100 values, so clashes are certain and each must be retried.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn ids_stay_unique_when_the_clock_does_not_move() {
    let env = account().await;
    // A symbol per task where it can, so the position locks do not line
    // the tasks up.
    let symbols = ["ZEEL", "RELIANCE", "SBIN", "INFY", "TCS", "HDFCBANK", "ITC"];
    for s in symbols {
        env.ltp(s, "NSE", "100");
    }
    let (tasks, per_task) = (8, 20);
    let barrier = Arc::new(Barrier::new(tasks));
    let mut handles = Vec::new();
    for i in 0..tasks {
        let (sb, barrier) = (env.sb.clone(), barrier.clone());
        let symbol = symbols[i % symbols.len()];
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let mut ids = Vec::new();
            for _ in 0..per_task {
                let placed = sb.place_order(resting_buy(symbol, 1, "90")).await;
                ids.push(placed.map(|p| p.orderid).map_err(|e| e.message));
            }
            ids
        }));
    }
    let mut ids = Vec::new();
    for h in handles {
        for id in h.await.unwrap() {
            ids.push(id.expect("every order is placed"));
        }
    }
    assert_eq!(ids.len(), tasks * per_task);
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "two orders were given the same id");
    // Web `test_ids_keep_their_format`: fourteen digits, YYMMDD first.
    for id in &ids {
        assert!(
            id.len() == 14 && id.bytes().all(|b| b.is_ascii_digit()) && id.starts_with("261012"),
            "{id}"
        );
    }
    assert_eq!(orders(&env).len(), tasks * per_task);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

/// Web `test_an_id_already_stored_is_skipped`: every id the first try can
/// draw at this instant is already stored (a restart, or an id issued
/// earlier); the order is still placed, under an id nobody has.
#[tokio::test]
async fn an_id_already_stored_is_skipped() {
    let env = account().await;
    let taken: HashSet<String> = (0..100).map(|n| format!("26101200000{n:03}")).collect();
    for id in &taken {
        store_order(&env, id);
    }
    env.ltp("SBIN", "NSE", "100");
    let id = env
        .sb
        .place_order(resting_buy("SBIN", 1, "90"))
        .await
        .unwrap()
        .orderid;
    assert!(!taken.contains(&id), "{id} was taken");
    assert!(id.len() == 14 && id.starts_with("261012"), "{id}");
    assert_eq!(orders(&env).len(), 101);
    env.shutdown().await;
}

// ------------------------------------------------------------ margin

/// Web `test_an_insert_that_fails_blocks_no_margin`: the order row cannot
/// be written because its id is taken, every try.
#[tokio::test]
async fn an_insert_that_fails_blocks_no_margin() {
    let env = account().await;
    env.ltp("RELIANCE", "NSE", "2600");
    refuse_inserts(&env, "UNIQUE constraint failed: sandbox_orders.orderid");
    let err = env
        .sb
        .place_order(resting_buy("RELIANCE", 10, "2500"))
        .await
        .unwrap_err();
    assert_eq!(err.http_status, 500, "{}", err.message);
    assert!(orders(&env).is_empty());
    assert_untouched(&env).await;
    env.shutdown().await;
}

/// Web `test_a_failure_before_the_insert_blocks_no_margin`: any other
/// failure after the margin was worked out and before the order row is
/// written.
#[tokio::test]
async fn a_failure_after_the_margin_blocks_no_margin() {
    let env = account().await;
    env.ltp("RELIANCE", "NSE", "2600");
    refuse_inserts(&env, "database unavailable");
    let err = env
        .sb
        .place_order(resting_buy("RELIANCE", 10, "2500"))
        .await
        .unwrap_err();
    assert_eq!(err.http_status, 500, "{}", err.message);
    assert!(orders(&env).is_empty());
    assert_untouched(&env).await;
    env.shutdown().await;
}

/// Web `test_a_placed_order_still_holds_its_margin`.
#[tokio::test]
async fn a_placed_order_still_holds_its_margin() {
    let env = account().await;
    env.ltp("RELIANCE", "NSE", "2600");
    let id = env
        .sb
        .place_order(resting_buy("RELIANCE", 10, "2500"))
        .await
        .unwrap()
        .orderid;
    let rows = orders(&env);
    assert_eq!(rows.len(), 1);
    let (orderid, status, margin) = &rows[0];
    assert_eq!((orderid, status.as_str()), (&id, "open"));
    assert_eq!(*margin, d("25000"));
    assert_eq!(env.used().await, *margin);
    assert_eq!(env.available().await, d(CAPITAL) - *margin);
    env.shutdown().await;
}

/// Web `test_two_orders_for_the_last_funds_place_one_and_refuse_the_other`:
/// different symbols, so the position locks do not order them, racing for
/// funds that cover one. One is placed; the other is refused plainly
/// (400, "Insufficient funds"), never with a database error; funds exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_orders_for_the_last_funds_place_one_and_refuse_the_other() {
    for _round in 0..10 {
        let env = account().await;
        // Each needs 6,000,000 of the 10,000,000.
        env.ltp("RELIANCE", "NSE", "6100");
        env.ltp("ZEEL", "NSE", "6100");
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for symbol in ["RELIANCE", "ZEEL"] {
            let (sb, barrier) = (env.sb.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                sb.place_order(resting_buy(symbol, 1000, "6000")).await
            }));
        }
        let mut placed = Vec::new();
        let mut refused = Vec::new();
        for h in handles {
            match h.await.unwrap() {
                Ok(p) => placed.push(p.orderid),
                Err(e) => refused.push(e),
            }
        }
        assert_eq!((placed.len(), refused.len()), (1, 1));
        assert_eq!(refused[0].http_status, 400, "{}", refused[0].message);
        assert!(
            refused[0].message.contains("Insufficient funds"),
            "{}",
            refused[0].message
        );
        let rows = orders(&env);
        assert_eq!(rows.len(), 1, "the refused order is not recorded");
        assert_eq!((&rows[0].0, rows[0].2), (&placed[0], d("6000000")));
        assert_eq!(env.used().await, d("6000000"));
        assert_eq!(env.available().await, d("4000000"));
        env.shutdown().await;
    }
}
