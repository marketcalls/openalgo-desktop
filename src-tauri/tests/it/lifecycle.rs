//! Sleep and wake (ARCH-03, PLATFORM-05): a wall clock that moved further
//! than the monotonic one is a wake, and a wake reconnects the broker feed
//! at once (keeping its subscriptions) and asks the order reconciler for a
//! pass, instead of waiting for the stall watchdog's awake time.

use crate::webui_support::{H, USER};
use futures_util::StreamExt;
use openalgo_desktop_lib::brokers::mock::MockFeed;
use openalgo_desktop_lib::services::lifecycle::wake;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::TcpListener;

/// A feed server that accepts every connection and reads until it closes.
async fn feed_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((tcp, _)) = listener.accept().await {
            conns.spawn(async move {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                    while let Some(Ok(_)) = ws.next().await {}
                }
            });
        }
    });
    (url, task)
}

/// Poll `f` for up to ten seconds.
async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} never happened", what);
}

fn connects(h: &H) -> u64 {
    h.ctx.websocket.stats().connects.load(Ordering::Relaxed)
}

#[tokio::test]
async fn a_wake_reconnects_the_feed_and_asks_for_an_order_pass() {
    let h = H::new();
    let (url, srv) = feed_server().await;
    h.ctx
        .websocket
        .connect(Box::new(MockFeed::new(url)))
        .await
        .unwrap();
    until("the first connection", || connects(&h) == 1).await;
    let asked = h.ctx.reconciler.stats.requested.load(Ordering::Relaxed);

    wake::woke(&h.ctx, Duration::from_secs(3600)).await;
    until("the reconnection", || connects(&h) == 2).await;
    assert!(h.ctx.reconciler.stats.requested.load(Ordering::Relaxed) > asked);
    assert_eq!(h.ctx.websocket.stats().stalls.load(Ordering::Relaxed), 0);
    h.ctx.websocket.disconnect().await.unwrap();
    srv.abort();
}

/// The owned watch notices the wall clock (the injected one) moving two
/// minutes in one tick of the monotonic clock, and reconnects.
#[tokio::test]
async fn the_watch_notices_the_wall_clock_running_ahead() {
    let h = H::new();
    let (url, srv) = feed_server().await;
    h.ctx
        .websocket
        .connect(Box::new(MockFeed::new(url)))
        .await
        .unwrap();
    until("the first connection", || connects(&h) == 1).await;
    wake::start(&h.ctx);
    h.clock.advance(chrono::Duration::minutes(2));
    until("the reconnection", || connects(&h) == 2).await;
    h.ctx.websocket.disconnect().await.unwrap();
    srv.abort();
}

/// ARCH-03: with nothing running, strategies that wait for alerts are still
/// named when quitting: the keys lock at quit (KEY-01), so their alerts are
/// refused until the next sign-in.
#[tokio::test]
async fn quitting_names_the_strategies_waiting_for_alerts() {
    use openalgo_desktop_lib::chartink::store::NewStrategy;
    use openalgo_desktop_lib::services::lifecycle::quit::quit_warning;
    let h = H::new();
    h.setup();
    assert_eq!(quit_warning(&h.ctx).await, None, "nothing runs or waits");
    h.ctx
        .strategy
        .store
        .create_strategy(USER, &serde_json::json!({"name": "Breakout"}))
        .unwrap();
    let momentum = NewStrategy {
        name: "Momentum".into(),
        webhook_id: "wh-momentum".into(),
        user_id: USER.into(),
        is_intraday: true,
        start_time: Some("09:15".into()),
        end_time: Some("15:00".into()),
        squareoff_time: Some("15:15".into()),
    };
    h.ctx.chartink.store.create(&momentum, h.ctx.now()).unwrap();
    let w = quit_warning(&h.ctx).await.unwrap();
    assert_eq!(w.level, "waiting");
    assert_eq!(
        w.items[..2],
        [
            "Webhook alerts for Breakout (TradingView and other senders) are refused until you open OpenAlgo and sign in again.",
            "Chartink alerts for Momentum are refused until you open OpenAlgo and sign in again.",
        ]
    );
    assert!(w.items[2].starts_with("OpenAlgo locks its keys"));
}
