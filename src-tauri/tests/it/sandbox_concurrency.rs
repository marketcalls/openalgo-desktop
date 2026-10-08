//! Races, run on a multi-threaded runtime with barriers (ports of the web's
//! `test/test_gthread_sandbox_*.py`): every pair below must end in exactly
//! one outcome, with funds and books agreeing.

use crate::sandbox_support;

use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::events::EventBus;
use openalgo_desktop_lib::sandbox::clock::manual_clock_at;
use openalgo_desktop_lib::sandbox::{
    GttRequest, Quote, QuoteSource, Sandbox, SandboxDeps, SandboxOptions, SmartOrderRequest,
    StaticQuoteSource, SymbolKey, Tick,
};
use rust_decimal::Decimal;
use sandbox_support::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Barrier, Notify};

const T: &str = "2026-10-05 10:00:00";

async fn race<F, Fut, R>(n: usize, f: F) -> Vec<R>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let barrier = Arc::new(Barrier::new(n));
    let mut hs = Vec::new();
    for i in 0..n {
        let b = barrier.clone();
        let fut = f(i);
        hs.push(tokio::spawn(async move {
            b.wait().await;
            fut.await
        }));
    }
    let mut out = Vec::new();
    for h in hs {
        out.push(h.await.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn two_simultaneous_triggers_on_one_position_produce_exactly_one_fill() {
    for _round in 0..20 {
        let env = Env::at(T);
        env.ltp("SBIN", "NSE", "100");
        let id = place(
            &env,
            limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "MIS"), "95"),
        )
        .await;
        env.ltp("SBIN", "NSE", "94");
        let sb = env.sb.clone();
        race(8, |i| {
            let sb = sb.clone();
            async move {
                if i % 2 == 0 {
                    sb.on_tick(Tick::new("SBIN", "NSE", d("94")))
                        .await
                        .map(|_| ())
                } else {
                    sb.poll_once().await.map(|_| ())
                }
            }
        })
        .await;
        // Ticks that found the position busy retry on the next price.
        env.sb
            .on_tick(Tick::new("SBIN", "NSE", d("94")))
            .await
            .unwrap();
        assert_eq!(env.status(&id).await, "complete");
        let trades = env.sb.tradebook().await.unwrap();
        assert_eq!(trades.data.len(), 1, "one order, one fill");
        assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 10);
        env.assert_margin_consistent().await;
        env.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_cancels_of_one_order_release_its_margin_once() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "MIS"), "95"),
    )
    .await;
    assert_eq!(env.used().await, d("190"));
    let sb = env.sb.clone();
    let out = race(2, |_| {
        let sb = sb.clone();
        let id = id.clone();
        async move { sb.cancel_order(&id).await }
    })
    .await;
    assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 1);
    let err = out.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert_eq!(err.message, "Cannot cancel order in cancelled status");
    assert_eq!(env.used().await, d("0"));
    assert_eq!(
        env.available().await,
        d("10000000"),
        "released once, not twice"
    );
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_cancel_racing_a_fill_leaves_one_outcome() {
    for _ in 0..20 {
        let env = Env::at(T);
        env.ltp("SBIN", "NSE", "100");
        let id = place(
            &env,
            limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "MIS"), "95"),
        )
        .await;
        let sb = env.sb.clone();
        let out = race(2, |i| {
            let sb = sb.clone();
            let id = id.clone();
            async move {
                if i == 0 {
                    sb.cancel_order(&id).await.map(|_| ())
                } else {
                    sb.on_tick(Tick::new("SBIN", "NSE", d("94")))
                        .await
                        .map(|_| ())
                }
            }
        })
        .await;
        let status = env.status(&id).await;
        let trades = env.sb.tradebook().await.unwrap().data.len();
        match status.as_str() {
            "cancelled" => {
                assert_eq!(trades, 0);
                assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
                assert!(out[0].is_ok());
            }
            "complete" => {
                assert_eq!(trades, 1);
                assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 10);
                assert!(out[0].is_err(), "the cancel must report the fill");
            }
            "open" => {
                // The tick found the lock busy and the cancel had not started.
                assert!(out[0].is_err() || trades == 0);
            }
            other => panic!("unexpected status {other}"),
        }
        env.assert_margin_consistent().await;
        env.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_closers_do_not_reverse_the_position() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    let sb = env.sb.clone();
    let out = race(2, |_| {
        let sb = sb.clone();
        async move { sb.close_position("SBIN", "NSE", "MIS").await }
    })
    .await;
    assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        env.qty("SBIN", "NSE", "MIS").await,
        0,
        "closed, not reversed"
    );
    env.assert_margin_consistent().await;
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_cnc_sells_of_the_same_shares_cannot_both_pass() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "CNC")).await;
    let sb = env.sb.clone();
    let out = race(2, |_| {
        let sb = sb.clone();
        async move {
            sb.place_order(req("SBIN", "NSE", "SELL", 10, "MARKET", "CNC"))
                .await
        }
    })
    .await;
    assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(env.qty("SBIN", "NSE", "CNC").await, 0);
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_exit_smart_orders_do_not_reverse_the_position() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    let sb = env.sb.clone();
    race(2, |_| {
        let sb = sb.clone();
        async move {
            sb.place_smart_order(SmartOrderRequest {
                symbol: "SBIN".into(),
                exchange: "NSE".into(),
                product: "MIS".into(),
                action: "SELL".into(),
                quantity: 0,
                position_size: 0,
                ..Default::default()
            })
            .await
        }
    })
    .await;
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    assert_eq!(
        env.sb.tradebook().await.unwrap().data.len(),
        2,
        "one buy, one exit"
    );
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_blocks_at_once_both_land_or_one_is_refused() {
    let env = Env::at(T);
    // Each order needs 6,000,000 of 10,000,000.
    env.ltp("SBIN", "NSE", "6000");
    env.ltp("INFY", "NSE", "6000");
    let sb = env.sb.clone();
    let out = race(2, |i| {
        let sb = sb.clone();
        async move {
            let s = if i == 0 { "SBIN" } else { "INFY" };
            sb.place_order(req(s, "NSE", "BUY", 1000, "MARKET", "CNC"))
                .await
        }
    })
    .await;
    assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(env.used().await, d("6000000"));
    assert_eq!(env.available().await, d("4000000"));
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_t1_settlement_runs_once() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "CNC")).await;
    env.set_time("2026-10-06 00:00:05");
    let sb = env.sb.clone();
    let out = race(4, |_| {
        let sb = sb.clone();
        async move { sb.t1_settlement().await.unwrap() }
    })
    .await;
    assert_eq!(out.iter().sum::<usize>(), 1);
    assert_eq!(
        env.sb.holdings().await.unwrap().data.holdings[0].quantity,
        10
    );
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_the_primary_and_backup_sweeps_close_a_position_once() {
    let env = Env::at(T);
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.set_time("2026-10-05 15:15:00");
    let sb = env.sb.clone();
    race(3, |_| {
        let sb = sb.clone();
        async move { sb.square_off_now().await.unwrap() }
    })
    .await;
    let autos = env
        .sb
        .orderbook()
        .await
        .unwrap()
        .data
        .orders
        .into_iter()
        .filter(|o| o.strategy == "AUTO_SQUARE_OFF")
        .count();
    assert_eq!(autos, 1);
    assert_eq!(env.qty("SBIN", "NSE", "MIS").await, 0);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Lock behaviour with a quote source that can be held
// ---------------------------------------------------------------------------

/// Quotes from a table, but a held symbol blocks until released.
struct GatedQuotes {
    inner: StaticQuoteSource,
    gate: Notify,
    held: parking_lot::Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl QuoteSource for GatedQuotes {
    async fn quote(&self, symbol: &str, exchange: &str) -> Option<Quote> {
        let hold = self.held.lock().as_deref() == Some(symbol);
        if hold {
            self.gate.notified().await;
        }
        self.inner.quote(symbol, exchange).await
    }
    async fn quotes(&self, keys: &[SymbolKey]) -> HashMap<SymbolKey, Quote> {
        self.inner.quotes(keys).await
    }
}

fn gated_env() -> (Sandbox, Arc<GatedQuotes>, Arc<ManualClock>) {
    let clock = manual_clock_at(T);
    let q = Arc::new(GatedQuotes {
        inner: StaticQuoteSource::new(),
        gate: Notify::new(),
        held: parking_lot::Mutex::new(None),
    });
    let sb = Sandbox::in_memory(
        SandboxDeps {
            symbols: Arc::new(symbols()),
            quotes: q.clone(),
            clock: clock.clone(),
            bus: Some(Arc::new(EventBus::new())),
        },
        SandboxOptions {
            user_id: USER.into(),
            quote_retry_delays: vec![],
            ..SandboxOptions::default()
        },
    )
    .unwrap();
    (sb, q, clock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_orders_on_different_positions_do_not_wait_for_each_other() {
    let (sb, q, _c) = gated_env();
    q.inner.set_ltp("SBIN", "NSE", d("100"));
    q.inner.set_ltp("INFY", "NSE", d("100"));
    *q.held.lock() = Some("SBIN".into());
    let sb2 = sb.clone();
    let slow = tokio::spawn(async move {
        sb2.place_order(req("SBIN", "NSE", "BUY", 1, "MARKET", "MIS"))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let fast = tokio::time::timeout(
        Duration::from_secs(2),
        sb.place_order(req("INFY", "NSE", "BUY", 1, "MARKET", "MIS")),
    )
    .await
    .expect("an order on another position must not wait")
    .unwrap();
    assert!(!fast.orderid.is_empty());
    assert!(!slow.is_finished());
    *q.held.lock() = None;
    q.gate.notify_waiters();
    slow.await.unwrap().unwrap();
    assert_eq!(sb.lock_table_len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_leg_never_waits_for_another_order_on_its_position() {
    let (sb, q, _c) = gated_env();
    q.inner.set_ltp("ZEEL", "NSE", d("100"));
    let g = sb
        .place_gtt(GttRequest {
            trigger_type: "SINGLE".into(),
            symbol: "ZEEL".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            quantity: 1,
            pricetype: "LIMIT".into(),
            price: Some(d("95")),
            triggerprice_sl: Some(d("95")),
            ..Default::default()
        })
        .await
        .unwrap();
    *q.held.lock() = Some("ZEEL".into());
    let sb2 = sb.clone();
    let order = tokio::spawn(async move {
        sb2.place_order(req("ZEEL", "NSE", "BUY", 1, "MARKET", "CNC"))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let fired = tokio::time::timeout(
        Duration::from_secs(2),
        sb.on_tick(Tick::new("ZEEL", "NSE", d("94"))),
    )
    .await
    .expect("the tick path never waits on a position lock")
    .unwrap()
    .1;
    assert_eq!(fired, 0, "the claim is handed back for the next tick");
    let book = sb.gtt_orderbook(Some("active")).await.unwrap();
    assert_eq!(book.data.len(), 1);
    assert_eq!(book.data[0].trigger_id, g.trigger_id);
    *q.held.lock() = None;
    q.gate.notify_waiters();
    order.await.unwrap().unwrap();
    q.inner.set_ltp("ZEEL", "NSE", d("94"));
    assert_eq!(
        sb.on_tick(Tick::new("ZEEL", "NSE", d("94")))
            .await
            .unwrap()
            .1,
        1
    );
    assert_eq!(sb.margin_discrepancy().await.unwrap(), Decimal::ZERO);
}
