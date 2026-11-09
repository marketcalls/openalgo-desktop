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
    async fn subscribe(&self, keys: &[Key]) -> Vec<Key> {
        self.subs.lock().extend_from_slice(keys);
        keys.to_vec()
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
        a.ctx.strategy.task_count() >= 4,
        "tick reader and evaluator, checkpoint, scheduler"
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
    assert!(
        t.m.tick_contexts.is_empty(),
        "every run's tick context forgotten"
    );
    assert_eq!(t.m.broadcast.tracked(), 0);
    assert_eq!(t.m.feed.runs_tracked(), 0);
    assert!(t.m.order_events.is_empty());
}

/// A strike of the master other than the at-the-money one.
const OTHER_CE: &str = "NIFTY13OCT2624550CE";

/// A feed client's reference to `symbol` (LTP, 5 levels).
fn client_reference(
    a: &App,
    symbol: &str,
) -> openalgo_desktop_lib::brokers::common::streaming::FeedSubscription {
    let row = a
        .ctx
        .symbols
        .by_symbol("NFO", symbol)
        .expect("in the master");
    openalgo_desktop_lib::brokers::common::streaming::FeedSubscription {
        symbol: symbol.into(),
        exchange: "NFO".into(),
        token: row.token.clone(),
        brsymbol: row.br_symbol().to_string(),
        brexchange: row.br_exchange().to_string(),
        mode: openalgo_desktop_lib::brokers::common::streaming::FeedMode::Ltp,
        depth: 5,
    }
}

/// Review X6: a run whose instruments the feed partly refused takes what
/// it can, and when it ends releases only that: another holder of the same
/// instrument keeps streaming (its reference was given back for the run's).
#[tokio::test]
async fn a_run_releases_only_what_the_feed_took() {
    use openalgo_desktop_lib::strategy::tick_feed::FeedPrices;
    use openalgo_desktop_lib::websocket::{FeedSubscription, Reader};
    let a = app();
    let feed = a.ctx.websocket.clone();
    feed.subscribe(vec![client_reference(&a, ATM_CE)])
        .await
        .unwrap();
    feed.set_refusal(Some(Arc::new(|s: &FeedSubscription| {
        (s.symbol == OTHER_CE).then(|| "The broker refused this instrument.".to_string())
    })));
    let prices = FeedPrices::new(Arc::downgrade(&a.ctx), feed.clone(), Reader::Strategy);
    let run = TickFeed::new(Some(Arc::new(prices)));
    let keys = [
        (ATM_CE.to_string(), "NFO".to_string()),
        (OTHER_CE.to_string(), "NFO".to_string()),
    ];
    run.add_run(1, &keys).await;
    run.remove_run(1).await;
    let held: Vec<String> = feed.subscriptions().into_iter().map(|s| s.symbol).collect();
    assert_eq!(held, [ATM_CE.to_string()], "the client's reference stays");
}

/// Review X6, for a scalping stop: a price the feed refused holds nothing,
/// so removing the stop does not take another holder's reference.
#[tokio::test]
async fn a_scalping_stop_releases_only_what_the_feed_took() {
    use openalgo_desktop_lib::scalping::store::SlUpsert;
    use openalgo_desktop_lib::websocket::FeedSubscription;
    let a = app();
    let feed = a.ctx.websocket.clone();
    feed.subscribe(vec![client_reference(&a, ATM_CE)])
        .await
        .unwrap();
    feed.set_refusal(Some(Arc::new(|_: &FeedSubscription| {
        Some("The broker refused this instrument.".to_string())
    })));
    let scalping = a.ctx.scalping.clone();
    let stop = SlUpsert {
        symbol: ATM_CE.into(),
        exchange: "NFO".into(),
        product: "MIS".into(),
        mode: "live".into(),
        side: Some("BUY".into()),
        entry_price: Some(100.0),
        quantity: Some(65),
        initial_sl: Some(95.0),
        is_active: Some(true),
        ..Default::default()
    };
    scalping.store.upsert_sl(&stop).unwrap();
    assert!(scalping.monitor.sync().await);
    scalping
        .store
        .delete_sl(ATM_CE, "NFO", "MIS", Some("live"))
        .unwrap();
    assert!(scalping.monitor.sync().await);
    let held: Vec<String> = feed.subscriptions().into_iter().map(|s| s.symbol).collect();
    assert_eq!(held, [ATM_CE.to_string()], "the client's reference stays");
}
