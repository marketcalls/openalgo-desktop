//! Owned tasks, bounded registries, subscriptions released.

use super::*;
use openalgo_desktop_lib::strategy::tick_feed::{Key, PriceSource, TickFeed};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
struct Prices {
    subs: Mutex<Vec<Key>>,
    unsubs: Mutex<Vec<Key>>,
    tx: Mutex<
        Option<
            tokio::sync::broadcast::Sender<
                openalgo_desktop_lib::brokers::common::streaming::MarketEvent,
            >,
        >,
    >,
}

#[async_trait::async_trait]
impl PriceSource for Prices {
    async fn subscribe(&self, keys: &[Key]) {
        self.subs.lock().extend_from_slice(keys);
    }
    async fn unsubscribe(&self, keys: &[Key]) {
        self.unsubs.lock().extend_from_slice(keys);
    }
    fn ticks(
        &self,
    ) -> tokio::sync::broadcast::Receiver<
        openalgo_desktop_lib::brokers::common::streaming::MarketEvent,
    > {
        let mut g = self.tx.lock();
        if g.is_none() {
            *g = Some(tokio::sync::broadcast::channel(64).0);
        }
        g.as_ref().unwrap().subscribe()
    }
    async fn poll(&self, _keys: &[Key]) -> Vec<(Key, f64)> {
        vec![]
    }
}

#[tokio::test]
async fn subscriptions_are_refcounted_per_run_and_released() {
    let p = Arc::new(Prices::default());
    let feed = TickFeed::new(Some(p.clone()));
    let k = ("X".to_string(), "NFO".to_string());
    feed.add_run(1, std::slice::from_ref(&k)).await;
    feed.add_run(2, std::slice::from_ref(&k)).await;
    assert_eq!(p.subs.lock().len(), 1, "one subscription for two runs");
    feed.remove_run(1).await;
    assert!(p.unsubs.lock().is_empty());
    feed.remove_run(2).await;
    assert_eq!(p.unsubs.lock().len(), 1);
    assert!(feed.subscribed().is_empty());
    assert_eq!(feed.runs_tracked(), 0);
}

#[tokio::test]
async fn background_tasks_are_owned_and_stopped() {
    let t = t();
    t.m.start().await;
    assert!(t.m.task_count() >= 2, "checkpoint and scheduler tasks");
    t.m.shutdown().await;
    assert_eq!(t.m.task_count(), 0);
}

#[tokio::test]
async fn the_app_shutdown_stops_the_strategy_module() {
    let a = app();
    a.ctx.strategy.start().await;
    assert!(
        a.ctx.strategy.task_count() >= 3,
        "tick consumer, checkpoint, scheduler"
    );
    a.ctx.shutdown().await;
    assert_eq!(a.ctx.strategy.task_count(), 0);
}

#[tokio::test]
async fn a_hundred_runs_leave_no_state_locks_or_throttle_entries() {
    let t = t();
    t.rooms
        .watching
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let sid = t.default_strategy();
    for _ in 0..100 {
        let run = t.start_filled(sid, 100.0).await;
        t.m.process_tick(ATM_CE, "NFO", 101.0).await;
        t.m.stop_run(run, USER, "manual").await;
        t.fill_last_exit(run, 100.0).await;
        assert!(t.run(run).stopped_at.is_some());
        t.m.webhook.advance(std::time::Duration::from_secs(31));
    }
    assert!(t.m.state.is_empty());
    assert_eq!(t.m.broadcast.tracked(), 0);
    assert_eq!(t.m.feed.runs_tracked(), 0);
    assert!(t.m.order_events.is_empty());
}
