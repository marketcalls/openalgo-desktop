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

/// The instruments the feed carries now, by symbol.
fn carried(a: &App) -> Vec<String> {
    let mut held: Vec<String> = a
        .ctx
        .websocket
        .subscriptions()
        .into_iter()
        .map(|s| s.symbol)
        .collect();
    held.sort();
    held
}

/// A live scalping stop on the at-the-money call.
fn atm_stop() -> openalgo_desktop_lib::scalping::store::SlUpsert {
    openalgo_desktop_lib::scalping::store::SlUpsert {
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
    }
}

/// A new session for the logged-in broker, started as a sign-in that ends
/// no session starts it (persist, then the runtime's activate; SES-02).
/// The session's start downloads the same master. The feed forgets every
/// reference before it returns.
async fn new_broker_session(a: &App) {
    *a.mock.master.lock() = Some(Ok(master()));
    let session = a.ctx.get_broker_session().expect("the mock session");
    a.ctx.runtime.activate(&a.ctx, &session).await;
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
    assert_eq!(carried(&a), [ATM_CE], "the client's reference stays");
}

/// Review X6, for a scalping stop: a price the feed refused holds nothing,
/// so removing the stop does not take another holder's reference.
#[tokio::test]
async fn a_scalping_stop_releases_only_what_the_feed_took() {
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
    scalping.store.upsert_sl(&atm_stop()).unwrap();
    assert!(scalping.monitor.sync().await);
    scalping
        .store
        .delete_sl(ATM_CE, "NFO", "MIS", Some("live"))
        .unwrap();
    assert!(scalping.monitor.sync().await);
    assert_eq!(carried(&a), [ATM_CE], "the client's reference stays");
}

/// Web test/test_websocket_cache_invalidation.py::test_internal_client_restores_subscriptions_after_reauthentication
/// (marketcalls/openalgo#2277, 9512aa5fe): what a run follows is taken
/// again in a new broker session. The web's internal client, which its
/// strategies read, holds RELIANCE in LTP and Quote and TCS in Quote and
/// sends one subscribe per mode once it has authenticated again; a run
/// follows prices in LTP only, so here the two instruments a run follows
/// are subscribed again. Once the run ends the feed carries nothing: what
/// was taken again belongs to the new session and goes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_broker_session_takes_a_runs_prices_again() {
    let a = app();
    a.ctx.strategy.start().await;
    let keys = [
        (ATM_CE.to_string(), "NFO".to_string()),
        (OTHER_CE.to_string(), "NFO".to_string()),
    ];
    a.ctx.strategy.feed.add_run(1, &keys).await;
    assert_eq!(carried(&a), [ATM_CE, OTHER_CE]);

    new_broker_session(&a).await;
    assert!(
        crate::feed_support::eventually(std::time::Duration::from_secs(10), || carried(&a)
            == [ATM_CE, OTHER_CE])
        .await,
        "taken again in the new session: {:?}",
        carried(&a)
    );
    a.ctx.strategy.feed.remove_run(1).await;
    assert!(carried(&a).is_empty(), "{:?}", carried(&a));
    a.ctx.shutdown().await;
}

/// What web test/test_websocket_cache_invalidation.py::test_invalidated_client_does_not_disconnect_replacement_adapter
/// pins (marketcalls/openalgo#2277, 9512aa5fe), for a run: one that ends
/// after a new broker session gives back nothing it took in the session
/// before, which the feed forgot with it. Another holder that took the
/// instrument in the new session (an 8765 client here; a Flow node or a
/// scalping stop alike) keeps streaming it. No renewal runs here: the run
/// ends before any.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_ending_after_a_new_broker_session_leaves_another_holders_price() {
    use openalgo_desktop_lib::strategy::tick_feed::FeedPrices;
    use openalgo_desktop_lib::websocket::Reader;
    let a = app();
    let feed = a.ctx.websocket.clone();
    let prices = FeedPrices::new(Arc::downgrade(&a.ctx), feed.clone(), Reader::Strategy);
    let run = TickFeed::new(Some(Arc::new(prices)));
    run.add_run(1, &[(ATM_CE.to_string(), "NFO".to_string())])
        .await;
    assert_eq!(carried(&a), [ATM_CE]);

    new_broker_session(&a).await;
    feed.subscribe(vec![client_reference(&a, ATM_CE)])
        .await
        .unwrap();
    run.remove_run(1).await;
    assert_eq!(carried(&a), [ATM_CE], "the client's reference stays");
    a.ctx.shutdown().await;
}

/// The sandbox's form of the test above (integration review of 1.0.10):
/// the sandbox's feed worker follows analyzer mode, not the sign-in, so a
/// new broker session does not restart it. The symbol it took for an order
/// before the session changed, released once the order ends, leaves the
/// price another holder took in the new session streaming. It gave that
/// holder's reference back before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sandbox_releasing_after_a_new_broker_session_leaves_another_holders_price() {
    use openalgo_desktop_lib::sandbox::{SymbolKey, TickSource};
    use openalgo_desktop_lib::services::sandbox_feed::FeedTicks;
    use std::time::Duration;
    let a = app();
    let ticks = FeedTicks::start(&a.ctx);
    let atm = SymbolKey::new(ATM_CE, "NFO");
    ticks.watch(&atm);
    assert!(
        crate::feed_support::eventually(Duration::from_secs(10), || carried(&a) == [ATM_CE]).await
    );

    new_broker_session(&a).await;
    a.ctx
        .websocket
        .subscribe(vec![client_reference(&a, ATM_CE)])
        .await
        .unwrap();
    ticks.unwatch(&atm);
    // The worker makes its calls in order: once this one is carried, the
    // release before it has been made.
    ticks.watch(&SymbolKey::new(OTHER_CE, "NFO"));
    assert!(
        crate::feed_support::eventually(Duration::from_secs(10), || carried(&a)
            .contains(&OTHER_CE.to_string()))
        .await
    );
    assert_eq!(
        carried(&a),
        [ATM_CE, OTHER_CE],
        "the client's reference stays"
    );
    drop(ticks);
    a.ctx.shutdown().await;
}

/// Web 9512aa5fe (marketcalls/openalgo#2277), for a scalping stop: the
/// web's scalping monitor subscribes again once its client has
/// authenticated again. A new session for the same broker does not
/// restart the monitor (no session ended), and the stop's price is taken
/// again; removing the stop afterwards gives back what was taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_broker_session_takes_a_scalping_stops_price_again() {
    let a = app();
    let scalping = a.ctx.scalping.clone();
    // persist() announced the session: the monitor runs.
    assert!(
        crate::feed_support::eventually(std::time::Duration::from_secs(10), || scalping
            .monitor
            .is_running())
        .await
    );
    scalping.store.upsert_sl(&atm_stop()).unwrap();
    assert!(scalping.monitor.sync().await);
    assert_eq!(carried(&a), [ATM_CE]);

    new_broker_session(&a).await;
    assert!(
        crate::feed_support::eventually(std::time::Duration::from_secs(10), || carried(&a)
            == [ATM_CE])
        .await,
        "taken again in the new session: {:?}",
        carried(&a)
    );
    scalping
        .store
        .delete_sl(ATM_CE, "NFO", "MIS", Some("live"))
        .unwrap();
    assert!(scalping.monitor.sync().await);
    assert!(carried(&a).is_empty(), "{:?}", carried(&a));
    a.ctx.shutdown().await;
}
