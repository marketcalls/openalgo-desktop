//! The engine task: ticks drive fills, the watch set follows what needs
//! ticks, the polling fallback runs only while the feed is stale, the
//! schedule runs from the task's clock, and stopping releases everything.

use crate::sandbox_support;

use openalgo_desktop_lib::sandbox::{BroadcastTicks, SandboxOptions, SymbolKey, Tick, TickSource};
use sandbox_support::*;
use std::sync::Arc;
use std::time::Duration;

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..400 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn sbin() -> SymbolKey {
    SymbolKey::new("SBIN", "NSE")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticks_fill_resting_orders_and_the_watch_set_follows_the_books() {
    let env = Env::at("2026-10-05 10:00:00");
    let ticks = Arc::new(BroadcastTicks::new(256));
    env.sb.start_engine(ticks.clone()).await.unwrap();
    assert!(env.sb.is_engine_running().await);

    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "MIS"), "95"),
    )
    .await;
    eventually("SBIN watched", || async {
        ticks.watched().contains(&sbin())
    })
    .await;

    // A tick for a symbol nobody needs is ignored.
    ticks.send(Tick::new("INFY", "NSE", d("1")));
    ticks.send(Tick::new("SBIN", "NSE", d("96")));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(env.status(&id).await, "open");
    ticks.send(Tick::new("SBIN", "NSE", d("95")));
    eventually("the limit fills from the tick", || async {
        env.status(&id).await == "complete"
    })
    .await;
    // The position is open, so SBIN stays watched for MTM.
    assert!(ticks.watched().contains(&sbin()));

    // Close it: nothing needs SBIN any more and the feed is released.
    env.ltp("SBIN", "NSE", "97");
    env.sb.close_position("SBIN", "NSE", "MIS").await.unwrap();
    ticks.send(Tick::new("SBIN", "NSE", d("97")));
    eventually("SBIN released", || async {
        !ticks.watched().contains(&sbin())
    })
    .await;

    // A cancelled order releases its symbol too.
    let c = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "50"),
    )
    .await;
    eventually("SBIN watched again", || async {
        ticks.watched().contains(&sbin())
    })
    .await;
    env.sb.cancel_order(&c).await.unwrap();
    eventually("SBIN released after cancel", || async {
        !ticks.watched().contains(&sbin())
    })
    .await;

    env.sb.stop_engine().await;
    assert!(!env.sb.is_engine_running().await);
    assert!(
        ticks.watched().is_empty(),
        "a stopped engine leaves no subscriptions behind"
    );
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtm_follows_ticks() {
    let env = Env::at("2026-10-05 10:00:00");
    let ticks = Arc::new(BroadcastTicks::new(256));
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.sb.start_engine(ticks.clone()).await.unwrap();
    eventually("SBIN watched for MTM", || async {
        ticks.watched().contains(&sbin())
    })
    .await;
    ticks.send(Tick::new("SBIN", "NSE", d("103")));
    eventually("position marked to market", || async {
        env.sb
            .position_row("SBIN", "NSE", "MIS")
            .await
            .unwrap()
            .map(|p| p.pnl == d("30"))
            .unwrap_or(false)
    })
    .await;
    let f = env.sb.funds_row().await.unwrap().unwrap();
    assert_eq!(f.unrealized_pnl, d("30"));
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_polling_fallback_runs_only_while_the_feed_is_stale() {
    // Fresh feed (30 s window): no tick arrives, but the engine started
    // moments ago, so it does not poll.
    let env = Env::at("2026-10-05 10:00:00");
    env.sb
        .update_config("order_check_interval", "1")
        .await
        .unwrap();
    let ticks = Arc::new(BroadcastTicks::new(16));
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "95"),
    )
    .await;
    env.sb.start_engine(ticks.clone()).await.unwrap();
    env.ltp("SBIN", "NSE", "94");
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert_eq!(
        env.status(&id).await,
        "open",
        "no polling while the feed counts as fresh"
    );
    env.shutdown().await;

    // Stale feed: the fallback polls quotes and fills.
    let env = Env::with_opts(
        "2026-10-05 10:00:00",
        SandboxOptions {
            stale_feed_after: Duration::ZERO,
            ..SandboxOptions::default()
        },
    );
    env.sb
        .update_config("order_check_interval", "1")
        .await
        .unwrap();
    let ticks = Arc::new(BroadcastTicks::new(16));
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "95"),
    )
    .await;
    env.sb.start_engine(ticks.clone()).await.unwrap();
    env.ltp("SBIN", "NSE", "94");
    eventually("the fallback fills", || async {
        env.status(&id).await == "complete"
    })
    .await;
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_runs_the_schedule_from_its_clock() {
    let env = Env::at("2026-10-05 10:00:00");
    let ticks = Arc::new(BroadcastTicks::new(16));
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.sb.start_engine(ticks.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    env.set_time("2026-10-05 15:15:10");
    eventually("auto square-off from the engine's clock", || async {
        env.qty("SBIN", "NSE", "MIS").await == 0
    })
    .await;
    let s = env.sb.squareoff_status().await;
    assert!(s.data.running);
    assert_eq!(s.data.timezone.as_deref(), Some("Asia/Kolkata"));
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analyzer_mode_on_catches_up_then_starts_and_off_stops() {
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "CNC")).await;
    env.set_time("2026-10-06 09:30:00");
    let ticks: Arc<dyn TickSource> = Arc::new(BroadcastTicks::new(16));
    env.sb.set_analyzer_mode(true, ticks.clone()).await.unwrap();
    assert!(env.sb.is_engine_running().await);
    assert_eq!(
        env.sb.holdings().await.unwrap().data.holdings.len(),
        1,
        "T+1 caught up on start"
    );
    env.sb.set_analyzer_mode(true, ticks.clone()).await.unwrap();
    env.sb.set_analyzer_mode(false, ticks).await.unwrap();
    assert!(!env.sb.is_engine_running().await);
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_pauses_and_resumes_a_running_engine() {
    let env = Env::at("2026-10-05 10:00:00");
    let ticks = Arc::new(BroadcastTicks::new(16));
    env.ltp("SBIN", "NSE", "100");
    place(&env, req("SBIN", "NSE", "BUY", 10, "MARKET", "MIS")).await;
    env.sb
        .update_config("equity_mis_leverage", "2")
        .await
        .unwrap();
    env.sb.start_engine(ticks.clone()).await.unwrap();
    let r = env.sb.reset().await.unwrap();
    assert!(r
        .message
        .starts_with("Configuration and data reset to defaults successfully."));
    assert!(
        env.sb.is_engine_running().await,
        "only what was running is restarted"
    );
    let f = env.sb.funds().await.unwrap();
    assert_eq!(f.data.reset_count, 1);
    assert_eq!(f.data.availablecash, 10_000_000.0);
    assert!(env.sb.orderbook().await.unwrap().data.orders.is_empty());
    assert_eq!(
        env.sb.config_value("equity_mis_leverage").await.unwrap(),
        "5"
    );
    env.shutdown().await;

    let env = Env::at("2026-10-05 10:00:00");
    env.sb.reset().await.unwrap();
    assert!(
        !env.sb.is_engine_running().await,
        "a stopped engine stays stopped"
    );
    env.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lagging_receiver_recovers_by_polling() {
    let env = Env::with_opts("2026-10-05 10:00:00", SandboxOptions::default());
    let ticks = Arc::new(BroadcastTicks::new(1));
    env.ltp("SBIN", "NSE", "100");
    let id = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 1, "LIMIT", "MIS"), "95"),
    )
    .await;
    env.sb.start_engine(ticks.clone()).await.unwrap();
    eventually("SBIN watched", || async {
        ticks.watched().contains(&sbin())
    })
    .await;
    env.ltp("SBIN", "NSE", "94");
    for i in 0..200 {
        ticks.send(Tick::new("INFY", "NSE", d(&format!("{}", 100 + i))));
    }
    ticks.send(Tick::new("SBIN", "NSE", d("94")));
    eventually("filled after the backlog", || async {
        env.status(&id).await == "complete"
    })
    .await;
    assert!(env.sb.is_engine_running().await);
    env.shutdown().await;
}

// SB-01: freshness is per symbol. Ticks for symbols the engine does not need
// (INFY) and for other watched symbols (TCS) must not hide that SBIN's own
// feed has stalled; SBIN's resting order is polled and fills from its quote.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_symbol_does_not_hide_a_stalled_one() {
    let env = Env::with_opts(
        "2026-10-05 10:00:00",
        SandboxOptions {
            stale_feed_after: Duration::from_millis(300),
            ..SandboxOptions::default()
        },
    );
    env.sb
        .update_config("order_check_interval", "1")
        .await
        .unwrap();
    let ticks = Arc::new(BroadcastTicks::new(256));
    let tcs = SymbolKey::new("TCS", "NSE");
    env.ltp("SBIN", "NSE", "100");
    env.ltp("TCS", "NSE", "3000");
    let sbin_order = place(
        &env,
        limit(req("SBIN", "NSE", "BUY", 10, "LIMIT", "MIS"), "95"),
    )
    .await;
    let tcs_order = place(
        &env,
        limit(req("TCS", "NSE", "BUY", 1, "LIMIT", "MIS"), "2000"),
    )
    .await;
    env.sb.start_engine(ticks.clone()).await.unwrap();
    eventually("SBIN and TCS watched", || async {
        let w = ticks.watched();
        w.contains(&sbin()) && w.contains(&tcs)
    })
    .await;

    let streaming = {
        let ticks = ticks.clone();
        tokio::spawn(async move {
            for i in (0u32..5).cycle() {
                ticks.send(Tick::new("INFY", "NSE", d("1500")));
                ticks.send(Tick::new("TCS", "NSE", d(&format!("{}", 3000 + i))));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };
    let calls_before = env.quotes.calls();
    // SBIN trades through the limit, but its feed sends nothing.
    env.ltp("SBIN", "NSE", "94");
    let started = std::time::Instant::now();
    let mut filled = false;
    while started.elapsed() < Duration::from_secs(4) {
        if env.status(&sbin_order).await == "complete" {
            filled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let waited = started.elapsed();
    let calls = env.quotes.calls() - calls_before;
    streaming.abort();
    assert!(
        filled,
        "SBIN's stalled feed must be polled while INFY and TCS keep ticking"
    );
    assert!(waited < Duration::from_secs(3), "filled after {waited:?}");
    assert!(
        calls <= 6,
        "quote calls stay bounded: {calls} in {waited:?}"
    );
    assert_eq!(
        env.status(&tcs_order).await,
        "open",
        "TCS is fresh and not re-quoted into a fill"
    );
    // The quiet symbol is subscribed again, with a backoff; the live ones
    // are left alone.
    let again = ticks.resubscribed(&sbin());
    assert!((1..=3).contains(&again), "SBIN re-subscribed {again} times");
    assert_eq!(ticks.resubscribed(&tcs), 0);
    env.shutdown().await;
}
