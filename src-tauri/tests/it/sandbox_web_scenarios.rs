//! Ports of the web's `test/sandbox/*` scenarios. Each test is named after
//! its web source; where the web's expectation depended on a web defect the
//! test says so.

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::db::HoldingRow;
use openalgo_desktop_lib::sandbox::{holdings, Quote, Tick};
use rust_decimal::Decimal;
use sandbox_support::*;

const T: &str = "2026-10-05 10:00:00";

async fn seed_holding(env: &Env, symbol: &str, qty: i64, avg: &str) {
    let (s, avg) = (symbol.to_string(), d(avg));
    env.sb
        .db()
        .with_tx(|tx| {
            holdings::insert(
                tx,
                &HoldingRow {
                    id: 0,
                    user_id: USER.into(),
                    symbol: s,
                    exchange: "NSE".into(),
                    quantity: qty,
                    average_price: avg,
                    ltp: Some(avg),
                    pnl: Decimal::ZERO,
                    pnl_percent: Decimal::ZERO,
                    settlement_date: "2026-10-04".into(),
                    created_at: "2026-10-04 00:00:00".into(),
                    updated_at: "2026-10-04 00:00:00".into(),
                },
            )
        })
        .unwrap();
}

// ---------------------------------------------------------------------------
// test/sandbox/test_margin_scenarios.py (ZEEL at 112.37, CNC 1x)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_scenario_1() {
    // BUY 100 -> SELL 50 -> SELL 50
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "112.37");
    place(&env, req("ZEEL", "NSE", "BUY", 100, "MARKET", "CNC")).await;
    assert_eq!(env.used().await, d("11237"));
    place(&env, req("ZEEL", "NSE", "SELL", 50, "MARKET", "CNC")).await;
    assert_eq!(env.used().await, d("5618.5"));
    place(&env, req("ZEEL", "NSE", "SELL", 50, "MARKET", "CNC")).await;
    assert_eq!(env.used().await, d("0"));
    assert_eq!(env.available().await, d("10000000"));
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn test_scenario_2() {
    // BUY 100 -> SELL 100 -> BUY 100 -> SELL 100
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "112.37");
    for _ in 0..2 {
        place(&env, req("ZEEL", "NSE", "BUY", 100, "MARKET", "CNC")).await;
        assert_eq!(env.used().await, d("11237"));
        place(&env, req("ZEEL", "NSE", "SELL", 100, "MARKET", "CNC")).await;
        assert_eq!(env.used().await, d("0"));
    }
    assert_eq!(env.available().await, d("10000000"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_scenario_3() {
    // BUY 100 -> SELL 200 in CNC. The web's own CNC sell check refuses the
    // 200 (only 100 available), so margin stays at 100 x 112.37, which is
    // what the web test asserts.
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "112.37");
    place(&env, req("ZEEL", "NSE", "BUY", 100, "MARKET", "CNC")).await;
    let e = env
        .sb
        .place_order(req("ZEEL", "NSE", "SELL", 200, "MARKET", "CNC"))
        .await
        .unwrap_err();
    assert!(e
        .message
        .starts_with("Cannot sell 200 shares of ZEEL in CNC. Only 100 shares available"));
    assert_eq!(env.used().await, d("11237"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_scenario_3_reversal_in_mis() {
    // The reversal the scenario meant: MIS BUY 100 -> SELL 200 leaves a
    // short 100 holding margin for 100 at the fill price.
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "112.37");
    place(&env, req("ZEEL", "NSE", "BUY", 100, "MARKET", "MIS")).await;
    assert_eq!(env.used().await, d("2247.4"));
    place(&env, req("ZEEL", "NSE", "SELL", 200, "MARKET", "MIS")).await;
    let p = env
        .sb
        .position_row("ZEEL", "NSE", "MIS")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.quantity, -100);
    assert_eq!(p.average_price, d("112.37"));
    assert_eq!(p.margin_blocked, d("2247.4"));
    assert_eq!(env.used().await, d("2247.4"));
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn test_scenario_4() {
    // BUY 100 -> BUY 100: margin doubles, weighted average.
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "112.37");
    place(&env, req("ZEEL", "NSE", "BUY", 100, "MARKET", "CNC")).await;
    env.ltp("ZEEL", "NSE", "114.37");
    place(&env, req("ZEEL", "NSE", "BUY", 100, "MARKET", "CNC")).await;
    assert_eq!(env.used().await, d("22674"));
    let p = env
        .sb
        .position_row("ZEEL", "NSE", "CNC")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.quantity, 200);
    assert_eq!(p.average_price, d("113.37"));
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// test/sandbox/test_fund_manager.py
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_fund_initialization() {
    let env = Env::at(T);
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.availablecash, 10_000_000.0);
    assert_eq!(f.data.utiliseddebits, 0.0);
    assert_eq!(f.data.reset_count, 0);
    assert_eq!(f.data.last_reset, "2026-10-05 10:00:00");
    env.shutdown().await;
}

#[tokio::test]
async fn test_insufficient_funds() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "1200");
    let e = env
        .sb
        .place_order(req("RELIANCE", "NSE", "BUY", 10_000, "MARKET", "CNC"))
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert!(
        e.message.starts_with("Insufficient funds. Required:"),
        "{}",
        e.message
    );
    assert_eq!(env.used().await, d("0"));
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// test/sandbox/test_cnc_sell_validation.py
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_cnc_sell_without_position() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    let e = env
        .sb
        .place_order(req("RELIANCE", "NSE", "SELL", 100, "MARKET", "CNC"))
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert!(e.message.contains("No positions or holdings available"));
    let orderid = e
        .orderid
        .clone()
        .expect("the rejection is recorded with an order id");
    let row = env.sb.order_row(&orderid).await.unwrap().unwrap();
    assert_eq!(row.order_status.as_str(), "rejected");
    assert_eq!(row.margin_blocked, Decimal::ZERO);
    assert_eq!(row.rejection_reason.as_deref(), Some(e.message.as_str()));
    env.shutdown().await;
}

#[tokio::test]
async fn test_cnc_sell_with_position() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    place(&env, req("RELIANCE", "NSE", "BUY", 100, "MARKET", "CNC")).await;
    let id = place(&env, req("RELIANCE", "NSE", "SELL", 50, "MARKET", "CNC")).await;
    assert_eq!(env.status(&id).await, "complete");
    assert_eq!(env.qty("RELIANCE", "NSE", "CNC").await, 50);
    env.shutdown().await;
}

#[tokio::test]
async fn test_cnc_sell_exceeding_position() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    place(&env, req("RELIANCE", "NSE", "BUY", 50, "MARKET", "CNC")).await;
    let e = env
        .sb
        .place_order(req("RELIANCE", "NSE", "SELL", 100, "MARKET", "CNC"))
        .await
        .unwrap_err();
    assert_eq!(
        e.message,
        "Cannot sell 100 shares of RELIANCE in CNC. Only 50 shares available (Position: 50, Holdings: 0)"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn test_cnc_sell_with_holdings() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    seed_holding(&env, "RELIANCE", 200, "2400").await;
    let id = place(&env, req("RELIANCE", "NSE", "SELL", 150, "MARKET", "CNC")).await;
    assert_eq!(env.status(&id).await, "complete");
    env.shutdown().await;
}

#[tokio::test]
async fn test_mis_short_selling() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    let id = place(&env, req("RELIANCE", "NSE", "SELL", 100, "MARKET", "MIS")).await;
    assert_eq!(env.status(&id).await, "complete");
    assert_eq!(env.qty("RELIANCE", "NSE", "MIS").await, -100);
    assert_eq!(env.used().await, d("50000"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_cnc_sell_with_position_and_holdings() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    place(&env, req("RELIANCE", "NSE", "BUY", 50, "MARKET", "CNC")).await;
    seed_holding(&env, "RELIANCE", 100, "2400").await;
    let id = place(&env, req("RELIANCE", "NSE", "SELL", 120, "MARKET", "CNC")).await;
    assert_eq!(env.status(&id).await, "complete");
    env.shutdown().await;
}

// test/sandbox/test_rejected_order.py and test_orderbook_api.py
#[tokio::test]
async fn test_rejected_order_appears_in_the_orderbook() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "112.37");
    let e = env
        .sb
        .place_order(req("ZEEL", "NSE", "SELL", 100, "MARKET", "CNC"))
        .await
        .unwrap_err();
    let ob = env.sb.orderbook().await.unwrap();
    assert_eq!(ob.data.orders.len(), 1);
    let o = &ob.data.orders[0];
    assert_eq!(o.order_status, "rejected");
    assert_eq!(o.rejection_reason, e.message);
    assert_eq!(
        o.price, 112.37,
        "the rejected MARKET row keeps the LTP it was priced at"
    );
    assert_eq!(ob.data.statistics.total_rejected_orders, 1);
    assert_eq!(ob.data.statistics.total_sell_orders, 1);
    env.rec.wait_for("order.update", 1).await;
    assert_eq!(env.rec.statuses(&o.orderid), vec!["rejected"]);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// test/sandbox/test_holdings_sell.py (issue #1640)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_partial_holdings_sell() {
    let env = Env::at(T);
    seed_holding(&env, "RELIANCE", 200, "2400").await;
    env.ltp("RELIANCE", "NSE", "2500");
    place(&env, req("RELIANCE", "NSE", "SELL", 150, "MARKET", "CNC")).await;
    assert_eq!(
        env.qty("RELIANCE", "NSE", "CNC").await,
        -150,
        "the day position records the sale"
    );
    let h = env.sb.holdings().await.unwrap();
    assert_eq!(
        h.data.holdings[0].quantity, 200,
        "only T+1 reduces the holding"
    );
    assert_eq!(
        env.available().await,
        d("10000000"),
        "proceeds arrive at settlement"
    );
    // T+1: the holding drops to 50 and the proceeds are credited.
    env.set_time("2026-10-06 00:00:05");
    assert_eq!(env.sb.t1_settlement().await.unwrap(), 1);
    let h = env.sb.holdings().await.unwrap();
    assert_eq!(h.data.holdings[0].quantity, 50);
    assert_eq!(h.data.holdings[0].average_price, 2400.0);
    assert_eq!(env.available().await, d("10375000"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_full_holdings_sell() {
    let env = Env::at(T);
    seed_holding(&env, "RELIANCE", 200, "2400").await;
    env.ltp("RELIANCE", "NSE", "2500");
    place(&env, req("RELIANCE", "NSE", "SELL", 200, "MARKET", "CNC")).await;
    assert_eq!(env.qty("RELIANCE", "NSE", "CNC").await, -200);
    assert_eq!(
        env.sb.holdings().await.unwrap().data.holdings[0].quantity,
        200
    );
    env.set_time("2026-10-06 00:00:05");
    env.sb.t1_settlement().await.unwrap();
    assert!(
        env.sb.holdings().await.unwrap().data.holdings.is_empty(),
        "a holding at 0 is deleted"
    );
    assert_eq!(env.available().await, d("10500000"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_position_then_holdings_sell() {
    let env = Env::at(T);
    env.ltp("RELIANCE", "NSE", "2500");
    place(&env, req("RELIANCE", "NSE", "BUY", 50, "MARKET", "CNC")).await;
    seed_holding(&env, "RELIANCE", 100, "2400").await;
    env.ltp("RELIANCE", "NSE", "2600");
    place(&env, req("RELIANCE", "NSE", "SELL", 120, "MARKET", "CNC")).await;
    let p = env
        .sb
        .position_row("RELIANCE", "NSE", "CNC")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.quantity, -70, "long 50 closed, 70 sold from the holding");
    assert_eq!(p.accumulated_realized_pnl, d("5000"));
    assert_eq!(
        env.sb.holdings().await.unwrap().data.holdings[0].quantity,
        100
    );
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// test/sandbox/test_concurrent_orders.py
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_same_symbol_buys_accumulate_position() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let mut handles = Vec::new();
    for _ in 0..10 {
        let sb = env.sb.clone();
        handles.push(tokio::spawn(async move {
            sb.place_order(req("ZEEL", "NSE", "BUY", 10, "MARKET", "MIS"))
                .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }
    assert_eq!(env.qty("ZEEL", "NSE", "MIS").await, 100);
    assert_eq!(env.used().await, d("2000"));
    assert_eq!(env.sb.tradebook().await.unwrap().data.len(), 10);
    env.assert_margin_consistent().await;
    assert_eq!(env.sb.lock_table_len(), 0, "the lock table empties");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// test/sandbox/test_execution_backlog.py
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_cycle_does_not_stall_on_a_large_backlog() {
    let env = Env::at(T);
    for i in 0..150 {
        let sym = ["ZEEL", "SBIN", "INFY"][i % 3];
        env.ltp(sym, "NSE", "100");
        place(&env, limit(req(sym, "NSE", "BUY", 1, "LIMIT", "MIS"), "90")).await;
    }
    let before = env.quotes.calls();
    for sym in ["ZEEL", "SBIN", "INFY"] {
        env.ltp(sym, "NSE", "89");
    }
    let stats = env.sb.poll_once().await.unwrap();
    assert_eq!(stats.pending, 150);
    assert_eq!(stats.filled, 150, "one pass clears the whole queue");
    assert_eq!(
        env.quotes.calls() - before,
        1,
        "one batch quote call per pass, flat in queue depth"
    );
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// services/sandbox_service.py::sandbox_cancel_all_orders
// ---------------------------------------------------------------------------

/// The web filters on `"trigger_pending"` (underscore) and so leaves every
/// resting SL and SL-M order uncancelled. Cancel-all must take them too.
#[tokio::test]
async fn test_cancel_all_cancels_trigger_pending_sl_orders_web_trigger_pending_bug() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "950");
    let lim = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "900"),
    )
    .await;
    let sl = place(
        &env,
        limit(
            trigger(req("SBIN", "NSE", "BUY", 1, "SL", "MIS"), "1000"),
            "1010",
        ),
    )
    .await;
    let slm = place(
        &env,
        trigger(req("SBIN", "NSE", "SELL", 1, "SL-M", "MIS"), "900"),
    )
    .await;
    assert_eq!(env.status(&sl).await, "trigger pending");
    assert_eq!(env.status(&slm).await, "trigger pending");
    let r = env.sb.cancel_all_orders().await.unwrap();
    assert_eq!(r.canceled_orders.len(), 3);
    assert_eq!(r.message, "Canceled 3 orders. Failed to cancel 0 orders.");
    for id in [&lim, &sl, &slm] {
        assert_eq!(env.status(id).await, "cancelled");
    }
    assert_eq!(
        env.used().await,
        d("0"),
        "every resting order's margin is back"
    );
    let again = env.sb.cancel_all_orders().await.unwrap();
    assert_eq!(again.message, "No open orders to cancel");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// test/sandbox/test_position_session_boundary.py
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_position_book_filters_on_the_session_boundary() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    env.ltp("NIFTY27OCT26FUT", "NFO", "22500");
    place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    place(&env, req("SBIN", "NSE", "SELL", 1, "MARKET", "MIS")).await;
    env.ltp("SBIN", "NSE", "110");
    place(&env, req("SBIN", "NSE", "BUY", 1, "MARKET", "CNC")).await;
    place(
        &env,
        req("NIFTY27OCT26FUT", "NFO", "BUY", 65, "MARKET", "NRML"),
    )
    .await;
    env.ltp("SBIN", "NSE", "112");
    env.sb
        .place_order(req("ZEEL", "NSE", "BUY", 1, "MARKET", "MIS"))
        .await
        .ok();
    let book = env.sb.positionbook().await.unwrap();
    assert_eq!(
        book.data.len(),
        2,
        "MIS round trip at zero P&L is hidden; CNC and NRML shown"
    );
    // Next day after 03:00: only the carried NRML remains in view (CNC moved
    // to holdings at T+1).
    env.set_time("2026-10-06 09:30:00");
    env.sb.t1_settlement().await.unwrap();
    let book = env.sb.positionbook().await.unwrap();
    let syms: Vec<&str> = book.data.iter().map(|p| p.symbol.as_str()).collect();
    assert_eq!(syms, vec!["NIFTY27OCT26FUT"]);
    env.shutdown().await;
}

#[tokio::test]
async fn a_closed_position_with_todays_pnl_stays_visible_until_the_boundary() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.ltp("SBIN", "NSE", "105");
    place(&env, req("SBIN", "NSE", "SELL", 10, "MARKET", "MIS")).await;
    let book = env.sb.positionbook().await.unwrap();
    assert_eq!(book.data.len(), 1);
    let p = &book.data[0];
    assert_eq!(p.quantity, 0);
    assert_eq!(
        p.average_price, 0.0,
        "closed rows show average 0, like the web"
    );
    assert_eq!(p.today_realized_pnl, 50.0);
    assert_eq!(p.pnl, 50.0);
    assert_eq!(book.total_today_realized_pnl, 50.0);
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.m2mrealized, 50.0);
    assert_eq!(f.data.availablecash, 10_000_050.0);
    env.set_time("2026-10-06 03:00:01");
    assert!(env.sb.positionbook().await.unwrap().data.is_empty());
    env.shutdown().await;
}

#[tokio::test]
async fn mtm_updates_position_pnl_and_funds_unrealized() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.quote(
        "SBIN",
        "NSE",
        Quote {
            ltp: d("103"),
            ..Quote::default()
        },
    );
    let book = env.sb.positionbook().await.unwrap();
    assert_eq!(book.data[0].ltp, 103.0);
    assert_eq!(book.data[0].unrealized_pnl, 30.0);
    assert_eq!(book.data[0].pnlpercent, 3.0);
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.m2munrealized, 30.0);
    assert_eq!(f.data.totalpnl, 30.0);
    let _ = env.sb.on_tick(Tick::new("SBIN", "NSE", d("104"))).await;
    env.shutdown().await;
}
