//! The sandbox's feed subscribe and release calls (web
//! `sandbox/websocket_execution_engine.py` `_feed_calls`,
//! marketcalls/openalgo#2192 5cf708a8a): the order path never waits for
//! the broker to answer one, the calls are made in the order they were
//! decided, and a failed one does not stop the next.
//!
//! The broker side is the real `WebSocketManager` with no socket: its
//! subscribe step records every subscribe and can hold it (a broker that
//! does not answer, for up to the manager's 10 s step deadline), and its
//! let-go hook records every release. The adapter is the production
//! `FeedTicks`.

use crate::sandbox_support::*;
use futures_util::FutureExt;
use openalgo_desktop_lib::brokers::common::streaming::{FeedLetGo, FeedSubscription, Presubscribe};
use openalgo_desktop_lib::brokers::common::symbols::SymToken;
use openalgo_desktop_lib::brokers::BrokerRegistry;
use openalgo_desktop_lib::clock::SystemClock;
use openalgo_desktop_lib::sandbox::{GttRequest, SymbolKey, TickSource};
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::sandbox_feed::FeedTicks;
use openalgo_desktop_lib::state::{AppState, OpenOptions};
use openalgo_desktop_lib::websocket::WebSocketManager;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, Notify};

/// How long an order may take while the feed is not answering (the web's
/// bound; a waited subscribe takes up to 55 s here).
const PROMPT: Duration = Duration::from_millis(1500);

fn nse(symbol: &str) -> SymbolKey {
    SymbolKey::new(symbol, "NSE")
}

/// The broker side of the feed: what it was asked, in order, and whether it
/// answers.
struct Broker {
    /// `("subscribe" | "release", symbol)`, in the order the broker heard
    /// them.
    calls: Mutex<Vec<(&'static str, String)>>,
    /// Symbols whose subscribe waits until [`Broker::answer`].
    held: Mutex<HashSet<String>>,
    /// Symbols whose subscribe the broker refuses.
    refused: Mutex<HashSet<String>>,
    asked: Notify,
    answering: watch::Sender<bool>,
}

impl Broker {
    fn on(feed: &WebSocketManager) -> Arc<Self> {
        let (answering, _) = watch::channel(false);
        let me = Arc::new(Self {
            calls: Mutex::default(),
            held: Mutex::default(),
            refused: Mutex::default(),
            asked: Notify::new(),
            answering,
        });
        let step: Presubscribe = {
            let me = me.clone();
            Arc::new(move |subs: Vec<FeedSubscription>| {
                let me = me.clone();
                async move { me.subscribe(subs).await }.boxed()
            })
        };
        feed.set_presubscribe(Some(step));
        let let_go: FeedLetGo = {
            let me = me.clone();
            Arc::new(move |subs: &[FeedSubscription]| {
                let mut calls = me.calls.lock();
                for s in subs {
                    calls.push(("release", s.symbol.clone()));
                }
            })
        };
        feed.set_let_go(Some(let_go));
        me
    }

    async fn subscribe(
        &self,
        subs: Vec<FeedSubscription>,
    ) -> Vec<Result<FeedSubscription, String>> {
        let wait = {
            let mut calls = self.calls.lock();
            let held = self.held.lock();
            let mut wait = false;
            for s in &subs {
                calls.push(("subscribe", s.symbol.clone()));
                wait |= held.contains(&s.symbol);
            }
            wait
        };
        if wait {
            self.asked.notify_one();
            let mut answering = self.answering.subscribe();
            let _ = answering.wait_for(|on| *on).await;
        }
        let refused = self.refused.lock();
        subs.into_iter()
            .map(|s| {
                if refused.contains(&s.symbol) {
                    Err("The broker refused this instrument.".to_string())
                } else {
                    Ok(s)
                }
            })
            .collect()
    }

    /// The broker stops answering these symbols' subscribes.
    fn hold(&self, symbols: &[&str]) {
        self.held
            .lock()
            .extend(symbols.iter().map(|s| s.to_string()));
    }

    fn refuse(&self, symbol: &str) {
        self.refused.lock().insert(symbol.to_string());
    }

    /// The broker answers again.
    fn answer(&self) {
        self.answering.send_replace(true);
    }

    /// Wait until the broker has been asked for a subscribe it holds.
    async fn until_asked(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.asked.notified())
            .await
            .expect("the broker was asked to subscribe");
    }

    /// What the broker heard for one symbol, in order.
    fn calls_for(&self, symbol: &str) -> Vec<&'static str> {
        self.calls
            .lock()
            .iter()
            .filter(|(_, s)| s == symbol)
            .map(|(kind, _)| *kind)
            .collect()
    }
}

fn listing(symbol: &str, n: usize) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: (10_000 + n).to_string(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

/// The production adapter on a feed whose broker side is a [`Broker`].
/// (Fields drop in order: the data folder goes after the app that has it
/// open.)
struct Rig {
    ticks: Arc<FeedTicks>,
    broker: Arc<Broker>,
    feed: Arc<WebSocketManager>,
    ctx: Arc<AppState>,
    _dir: tempfile::TempDir,
}

impl Rig {
    /// `symbols` listed on NSE in the symbol master, with `SETTLED`.
    fn new(symbols: &[String]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: Arc::new(SystemClock),
                brokers: Arc::new(BrokerRegistry::new()),
            },
        )
        .expect("open app state");
        let helpers = ["SETTLED"].map(String::from);
        ctx.symbols.load(
            symbols
                .iter()
                .chain(helpers.iter())
                .enumerate()
                .map(|(n, s)| listing(s, n))
                .collect(),
        );
        let feed = Arc::new(WebSocketManager::new());
        let broker = Broker::on(&feed);
        let ticks = FeedTicks::with_feed(&ctx, feed.clone());
        Self {
            ticks,
            broker,
            feed,
            ctx,
            _dir: dir,
        }
    }

    fn of(symbols: &[&str]) -> Self {
        Self::new(&symbols.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn watch(&self, symbol: &str) {
        self.ticks.watch(&nse(symbol));
    }

    fn unwatch(&self, symbol: &str) {
        self.ticks.unwatch(&nse(symbol));
    }

    fn subscribed(&self, symbol: &str) -> bool {
        self.feed.subscriptions().iter().any(|s| s.symbol == symbol)
    }

    async fn until_subscribed(&self, symbol: &str) {
        for _ in 0..1000 {
            if self.subscribed(symbol) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{symbol} was never subscribed");
    }

    /// Wait until every call decided so far has been made: the worker makes
    /// them oldest first, so a symbol watched now is subscribed only after
    /// them.
    async fn settle(&self) {
        self.watch("SETTLED");
        self.until_subscribed("SETTLED").await;
    }

    async fn close(self) {
        self.broker.answer();
        drop(self.ticks);
        self.ctx.shutdown().await;
    }
}

/// Web test/test_gthread_sandbox_lifecycle.py::test_placing_an_order_does_not_wait_for_the_feed_to_acknowledge
/// and ::test_no_order_path_hook_waits_for_the_feed (order_completed,
/// position_opened, gtt_placed); test/test_gthread_sandbox_eventlet.py's
/// test of the same name (issue #2104). Placing, cancelling and filling an
/// order and placing a GTT tell the engine without waiting, and the engine
/// hands the subscribe to the feed's own worker: none of them waits for a
/// broker that does not answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_order_path_waits_for_the_feed_to_acknowledge() {
    let rig = Rig::of(&["RELIANCE", "ZEEL", "SBIN"]);
    rig.broker.hold(&["RELIANCE", "ZEEL", "SBIN"]);
    let env = Env::at("2026-10-05 10:00:00");
    env.ltp("RELIANCE", "NSE", "1300");
    env.ltp("ZEEL", "NSE", "100");
    env.ltp("SBIN", "NSE", "800");
    env.sb.start_engine(rig.ticks.clone()).await.unwrap();

    let started = Instant::now();
    let resting = place(
        &env,
        limit(req("RELIANCE", "NSE", "BUY", 1, "LIMIT", "MIS"), "1200"),
    )
    .await;
    let placing = started.elapsed();
    // The subscribe reached the broker, which is not answering it.
    rig.broker.until_asked().await;
    assert!(placing < PROMPT, "placing the order waited {placing:?}");

    let started = Instant::now();
    env.sb.cancel_order(&resting).await.unwrap();
    let cancelling = started.elapsed();
    assert!(cancelling < PROMPT, "cancelling waited {cancelling:?}");

    let started = Instant::now();
    place(&env, req("ZEEL", "NSE", "BUY", 1, "MARKET", "MIS")).await;
    let filling = started.elapsed();
    assert!(
        filling < PROMPT,
        "a fill opening a position waited {filling:?}"
    );

    let started = Instant::now();
    env.sb
        .place_gtt(GttRequest {
            trigger_type: "SINGLE".into(),
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            quantity: 1,
            pricetype: "LIMIT".into(),
            price: Some(d("700")),
            triggerprice_sl: Some(d("700")),
            strategy: Some("gtt-test".into()),
            ..GttRequest::default()
        })
        .await
        .unwrap();
    let gtt = started.elapsed();
    assert!(gtt < PROMPT, "placing the GTT waited {gtt:?}");
    assert_eq!(
        rig.broker.calls_for("RELIANCE"),
        ["subscribe"],
        "everything above ran while the broker held that subscribe"
    );

    rig.broker.answer();
    env.sb.stop_engine().await;
    env.shutdown().await;
    rig.close().await;
}

/// Web test/test_gthread_sandbox_lifecycle.py::test_a_quick_place_then_cancel_unsubscribes_after_it_subscribes
/// (as 398f614cc left it): the release lands while the subscribe is on its
/// way to the broker, and follows it; it never overtakes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_release_while_the_subscribe_is_in_flight_follows_it() {
    let rig = Rig::of(&["RELIANCE"]);
    rig.broker.hold(&["RELIANCE"]);
    rig.watch("RELIANCE");
    rig.broker.until_asked().await;
    rig.unwatch("RELIANCE");
    rig.broker.answer();
    rig.settle().await;

    assert_eq!(rig.broker.calls_for("RELIANCE"), ["subscribe", "release"]);
    assert!(!rig.subscribed("RELIANCE"));
    rig.close().await;
}

/// Web test/test_gthread_sandbox_lifecycle.py::test_a_failed_subscribe_does_not_stop_the_next_one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_subscribe_does_not_stop_the_next_one() {
    let rig = Rig::of(&["RELIANCE", "ZEEL"]);
    rig.broker.refuse("RELIANCE");
    rig.watch("RELIANCE");
    rig.watch("ZEEL");
    rig.settle().await;

    assert_eq!(rig.broker.calls_for("RELIANCE"), ["subscribe"]);
    assert_eq!(rig.broker.calls_for("ZEEL"), ["subscribe"]);
    assert!(!rig.subscribed("RELIANCE"));
    assert!(rig.subscribed("ZEEL"));
    rig.close().await;
}
