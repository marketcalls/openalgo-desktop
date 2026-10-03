//! GTT in the sandbox: ports of `test/sandbox/test_gtt_manager.py` classes
//! that exercise the database (margin, claims, OCO exclusivity, stranded
//! legs, expiry, modify immutability, the order book contract) plus trigger
//! classes driven by ticks.

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::{GttRequest, Tick};
use rusqlite::params;
use rust_decimal::Decimal;
use sandbox_support::*;

const T: &str = "2026-10-05 10:00:00";

fn single(action: &str, trigger: &str, price: &str, qty: i64) -> GttRequest {
    GttRequest {
        trigger_type: "SINGLE".into(),
        symbol: "ZEEL".into(),
        exchange: "NSE".into(),
        action: action.into(),
        product: "CNC".into(),
        quantity: qty,
        pricetype: "LIMIT".into(),
        price: Some(d(price)),
        triggerprice_sl: Some(d(trigger)),
        strategy: Some("gtt-test".into()),
        ..GttRequest::default()
    }
}

fn oco() -> GttRequest {
    GttRequest {
        trigger_type: "OCO".into(),
        triggerprice_sl: Some(d("95")),
        stoploss: Some(d("94")),
        triggerprice_tg: Some(d("120")),
        target: Some(d("121")),
        ..single("SELL", "95", "95", 10)
    }
}

fn legs(env: &Env, trigger_id: &str) -> Vec<(i64, String)> {
    let id = trigger_id.to_string();
    env.sb
        .db()
        .with_conn(|c| -> rusqlite::Result<Vec<(i64, String)>> {
            let mut s = c.prepare(
                "SELECT id, leg_status FROM sandbox_gtt_legs WHERE gtt_id = ?1 ORDER BY leg_number",
            )?;
            let r = s.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            r.collect()
        })
        .unwrap()
}

fn gtt_status(env: &Env, trigger_id: &str) -> String {
    let id = trigger_id.to_string();
    env.sb
        .db()
        .with_conn(|c| {
            c.query_row(
                "SELECT gtt_status FROM sandbox_gtt WHERE gtt_id = ?1",
                params![id],
                |r| r.get(0),
            )
        })
        .unwrap()
}

// TestGTTMargin
#[tokio::test]
async fn test_placement_blocks_margin_and_cancel_returns_it_exactly() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env
        .sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    assert_eq!(r.status, "success");
    assert!(r.trigger_id.starts_with("GTT-261005-"));
    assert_eq!(env.used().await, d("950"));
    env.sb.cancel_gtt(&r.trigger_id).await.unwrap();
    assert_eq!(env.used().await, d("0"));
    assert_eq!(env.available().await, d("10000000"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_oco_blocks_the_larger_leg_not_both() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env.sb.place_gtt(oco()).await.unwrap();
    assert_eq!(env.used().await, d("1210"), "max(940, 1210)");
    let book = env.sb.gtt_orderbook(Some("active")).await.unwrap();
    assert_eq!(book.data[0].margin_blocked, 1210.0);
    assert_eq!(book.data[0].trigger_type, "two-leg");
    assert_eq!(book.data[0].trigger_prices, vec![95.0, 120.0]);
    env.sb.cancel_gtt(&r.trigger_id).await.unwrap();
    env.sb
        .update_config("gtt_oco_margin_mode", "sum")
        .await
        .unwrap();
    env.sb.place_gtt(oco()).await.unwrap();
    assert_eq!(env.used().await, d("2150"), "sum mode reserves both legs");
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn test_expiry_releases_margin() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let mut g = single("BUY", "95", "95", 10);
    g.expires_at = Some("2026-10-05T12:00:00".into());
    let r = env.sb.place_gtt(g).await.unwrap();
    assert_eq!(env.sb.gtt_maintenance().await.unwrap(), (0, 0));
    env.set_time("2026-10-05 12:00:01");
    assert_eq!(env.sb.gtt_maintenance().await.unwrap().1, 1);
    assert_eq!(gtt_status(&env, &r.trigger_id), "expired");
    assert_eq!(env.used().await, d("0"));
    env.rec.wait_for("gtt.expired", 1).await;
    env.shutdown().await;
}

#[tokio::test]
async fn test_cancelling_twice_does_not_double_release() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env
        .sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    env.sb.cancel_gtt(&r.trigger_id).await.unwrap();
    let e = env.sb.cancel_gtt(&r.trigger_id).await.unwrap_err();
    assert_eq!(e.http_status, 404);
    assert_eq!(
        e.message,
        format!("No active GTT with trigger_id '{}'", r.trigger_id)
    );
    assert_eq!(env.used().await, d("0"));
    env.shutdown().await;
}

#[tokio::test]
async fn test_a_placement_beyond_available_funds_is_refused() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let e = env
        .sb
        .place_gtt(single("BUY", "95", "95", 200_000))
        .await
        .unwrap_err();
    assert_eq!(e.http_status, 400);
    assert!(e.message.starts_with("Insufficient funds"));
    assert!(env.sb.gtt_orderbook(None).await.unwrap().data.is_empty());
    env.shutdown().await;
}

#[tokio::test]
async fn unknown_symbol_and_bad_shapes_are_refused_with_the_web_messages() {
    let env = Env::at(T);
    let mut g = single("BUY", "95", "95", 1);
    g.symbol = "NOTASYMBOL".into();
    assert_eq!(
        env.sb.place_gtt(g).await.unwrap_err().message,
        "Symbol not found"
    );
    let mut both = single("BUY", "95", "95", 1);
    both.triggerprice_tg = Some(d("110"));
    assert_eq!(
        env.sb.place_gtt(both).await.unwrap_err().message,
        "A SINGLE GTT needs exactly one of triggerprice_sl or triggerprice_tg; an OCO needs both."
    );
    env.shutdown().await;
}

// Trigger classes driven by ticks: BUY on dip, SELL at target, OCO.
#[tokio::test]
async fn a_buy_on_dip_fires_once_when_the_price_falls_and_places_its_order() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env
        .sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    assert_eq!(
        env.sb
            .on_tick(Tick::new("ZEEL", "NSE", d("96")))
            .await
            .unwrap(),
        (0, 0)
    );
    env.ltp("ZEEL", "NSE", "94.5");
    let (_, fired) = env
        .sb
        .on_tick(Tick::new("ZEEL", "NSE", d("94.5")))
        .await
        .unwrap();
    assert_eq!(fired, 1);
    assert_eq!(gtt_status(&env, &r.trigger_id), "triggered");
    let book = env.sb.gtt_orderbook(Some("triggered")).await.unwrap();
    let child = book.data[0].legs[0]
        .triggered_order_id
        .clone()
        .expect("the leg records its order");
    let o = env.sb.order_row(&child).await.unwrap().unwrap();
    assert_eq!(
        o.order_status.as_str(),
        "complete",
        "a marketable 95 limit fills at 94.5"
    );
    assert_eq!(o.average_price, Some(d("94.5")));
    assert_eq!(o.strategy.as_deref(), Some("gtt-test"));
    assert_eq!(o.gtt_leg_id, Some(legs(&env, &r.trigger_id)[0].0));
    assert_eq!(env.qty("ZEEL", "NSE", "CNC").await, 10);
    assert_eq!(
        env.used().await,
        d("950"),
        "the GTT reservation became the order's margin (at its limit)"
    );
    assert_eq!(
        env.sb
            .on_tick(Tick::new("ZEEL", "NSE", d("90")))
            .await
            .unwrap()
            .1,
        0,
        "fires once"
    );
    env.rec.wait_for("gtt.triggered", 1).await;
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test]
async fn an_oco_fires_one_leg_and_cancels_the_other() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    place(&env, req("ZEEL", "NSE", "BUY", 10, "MARKET", "CNC")).await;
    let r = env.sb.place_gtt(oco()).await.unwrap();
    env.ltp("ZEEL", "NSE", "121");
    assert_eq!(
        env.sb
            .on_tick(Tick::new("ZEEL", "NSE", d("121")))
            .await
            .unwrap()
            .1,
        1
    );
    let l = legs(&env, &r.trigger_id);
    assert_eq!(l[0].1, "cancelled");
    assert_eq!(l[1].1, "triggered");
    assert_eq!(
        env.qty("ZEEL", "NSE", "CNC").await,
        0,
        "the target leg sold the position"
    );
    assert_eq!(
        env.sb
            .on_tick(Tick::new("ZEEL", "NSE", d("90")))
            .await
            .unwrap()
            .1,
        0
    );
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// TestOCOExclusivity / TestClaimConcurrency
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_concurrent_sibling_contention_yields_one_winner() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env.sb.place_gtt(oco()).await.unwrap();
    let ids: Vec<i64> = legs(&env, &r.trigger_id)
        .iter()
        .map(|(id, _)| *id)
        .collect();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut hs = Vec::new();
    for i in 0..16 {
        let db = env.sb.db().clone();
        let leg = ids[i % 2];
        let b = barrier.clone();
        hs.push(tokio::spawn(async move {
            b.wait().await;
            let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 5)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap();
            tokio::task::spawn_blocking(move || {
                db.with_tx(|tx| openalgo_desktop_lib::sandbox::gtt::try_claim(tx, leg, now))
                    .unwrap()
            })
            .await
            .unwrap()
        }));
    }
    let mut wins = 0;
    for h in hs {
        if h.await.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(wins, 1, "exactly one evaluator owns an OCO");
    env.shutdown().await;
}

// TestStrandedLegReclaim
#[tokio::test]
async fn test_stale_claim_is_reverted_to_pending_and_a_fresh_one_left_alone() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env
        .sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    let leg = legs(&env, &r.trigger_id)[0].0;
    let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 5)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    assert!(env
        .sb
        .db()
        .with_tx(|tx| openalgo_desktop_lib::sandbox::gtt::try_claim(tx, leg, now))
        .unwrap());
    env.advance(30);
    assert_eq!(
        env.sb.gtt_maintenance().await.unwrap().0,
        0,
        "a 30 s old claim is left alone"
    );
    env.advance(31);
    assert_eq!(env.sb.gtt_maintenance().await.unwrap().0, 1);
    assert_eq!(legs(&env, &r.trigger_id)[0].1, "pending");
    env.shutdown().await;
}

// TestMarginReconciliation
#[tokio::test]
async fn test_active_gtt_margin_is_not_flagged_as_a_discrepancy() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    env.sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    place(&env, req("ZEEL", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    env.assert_margin_consistent().await;
    assert_eq!(env.used().await, d("970"));
    env.shutdown().await;
}

// TestCancelledGttCannotFire
#[tokio::test]
async fn test_fire_refuses_when_the_parent_is_not_active() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env
        .sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    env.sb.cancel_gtt(&r.trigger_id).await.unwrap();
    assert_eq!(
        env.sb
            .on_tick(Tick::new("ZEEL", "NSE", d("90")))
            .await
            .unwrap()
            .1,
        0
    );
    assert_eq!(legs(&env, &r.trigger_id)[0].1, "cancelled");
    assert!(env.sb.orderbook().await.unwrap().data.orders.is_empty());
    env.shutdown().await;
}

// TestRejectedOrdersDoNotPoisonCorrelation
#[tokio::test]
async fn test_rejected_child_is_not_correlated_and_the_gtt_rearms() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    // A CNC SELL GTT with nothing to sell: the child order is rejected.
    let r = env
        .sb
        .place_gtt(single("SELL", "95", "95", 10))
        .await
        .unwrap();
    assert_eq!(env.used().await, d("950"));
    env.ltp("ZEEL", "NSE", "94");
    assert_eq!(
        env.sb
            .on_tick(Tick::new("ZEEL", "NSE", d("94")))
            .await
            .unwrap()
            .1,
        0
    );
    assert_eq!(gtt_status(&env, &r.trigger_id), "active", "re-armed");
    assert_eq!(legs(&env, &r.trigger_id)[0].1, "pending");
    assert_eq!(env.used().await, d("950"), "its reservation restored");
    let ob = env.sb.orderbook().await.unwrap();
    assert_eq!(ob.data.orders[0].order_status, "rejected");
    let gtt_leg: Option<i64> = env
        .sb
        .order_row(&ob.data.orders[0].orderid)
        .await
        .unwrap()
        .unwrap()
        .gtt_leg_id;
    assert_eq!(gtt_leg, None);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// TestModifyImmutability and a legal modify
#[tokio::test]
async fn test_modify_immutability_and_margin_delta() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let r = env
        .sb
        .place_gtt(single("BUY", "95", "95", 10))
        .await
        .unwrap();
    let mut flip = single("SELL", "95", "95", 10);
    flip.symbol = "ZEEL".into();
    let e = env.sb.modify_gtt(&r.trigger_id, flip).await.unwrap_err();
    assert!(e.message.starts_with("action cannot be changed"));
    let mut sym = single("BUY", "95", "95", 10);
    sym.symbol = "SBIN".into();
    assert!(env
        .sb
        .modify_gtt(&r.trigger_id, sym)
        .await
        .unwrap_err()
        .message
        .starts_with("symbol cannot be changed"));
    let mut ty = oco();
    ty.action = "BUY".into();
    assert!(env
        .sb
        .modify_gtt(&r.trigger_id, ty)
        .await
        .unwrap_err()
        .message
        .starts_with("trigger_type cannot be changed"));
    env.sb
        .modify_gtt(&r.trigger_id, single("BUY", "90", "90", 20))
        .await
        .unwrap();
    assert_eq!(env.used().await, d("1800"));
    let book = env.sb.gtt_orderbook(Some("active")).await.unwrap();
    assert_eq!(book.data[0].trigger_prices, vec![90.0]);
    assert_eq!(book.data[0].legs[0].quantity, 20);
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

// TestOrderbookContract
#[tokio::test]
async fn test_cancelled_gtts_are_hidden_by_default() {
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let a = env
        .sb
        .place_gtt(single("BUY", "95", "95", 1))
        .await
        .unwrap();
    env.sb
        .place_gtt(single("BUY", "96", "96", 1))
        .await
        .unwrap();
    env.sb.cancel_gtt(&a.trigger_id).await.unwrap();
    assert_eq!(
        env.sb
            .gtt_orderbook(Some("active"))
            .await
            .unwrap()
            .data
            .len(),
        1
    );
    let all = env.sb.gtt_orderbook(None).await.unwrap();
    assert_eq!(all.data.len(), 2);
    let cancelled = all
        .data
        .iter()
        .find(|g| g.trigger_id == a.trigger_id)
        .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(cancelled.margin_blocked, 0.0);
    assert_eq!(cancelled.last_price, 100.0);
    assert!(cancelled.created_at.starts_with("2026-10-05T10:00:00"));
    assert!(
        cancelled.expires_at.starts_with("2027-10-05T10:00:00"),
        "default expiry is a year"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn a_market_leg_reserves_margin_at_its_trigger() {
    // TestMarketOrderMargin
    let env = Env::at(T);
    env.ltp("ZEEL", "NSE", "100");
    let mut g = single("BUY", "95", "0", 10);
    g.pricetype = "MARKET".into();
    g.price = None;
    env.sb.place_gtt(g).await.unwrap();
    assert_eq!(env.used().await, d("950"));
    assert!(Decimal::ZERO < env.used().await);
    env.shutdown().await;
}
