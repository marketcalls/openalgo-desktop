//! The engine task's commands, against an in-memory sandbox and the real
//! engine task: what it asks the feed for when commands reach it in a
//! different order from the commits they report.

use crate::sandbox::clock::manual_clock_at;
use crate::sandbox::{
    BroadcastTicks, EngineCmd, OrderRequest, Sandbox, SandboxDeps, SandboxOptions,
    StaticQuoteSource, StaticSymbols, SymbolKey, Tick, TickSource,
};
use parking_lot::Mutex;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// The feed side of the engine: every call it was asked for, in order, as
/// `(call, symbol)`. A `watch` is what queues a subscribe at the broker.
struct FeedLog {
    ticks: BroadcastTicks,
    calls: Mutex<Vec<(&'static str, String)>>,
}

impl FeedLog {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ticks: BroadcastTicks::new(16),
            calls: Mutex::default(),
        })
    }

    fn record(&self, call: &'static str, key: &SymbolKey) {
        self.calls.lock().push((call, key.symbol.clone()));
    }

    fn calls(&self) -> Vec<(&'static str, String)> {
        self.calls.lock().clone()
    }
}

impl TickSource for FeedLog {
    fn subscribe_ticks(&self) -> broadcast::Receiver<Tick> {
        self.ticks.subscribe_ticks()
    }

    fn watch(&self, key: &SymbolKey) {
        self.record("watch", key);
    }

    fn unwatch(&self, key: &SymbolKey) {
        self.record("release", key);
    }

    fn resubscribe(&self, key: &SymbolKey) {
        self.record("resubscribe", key);
    }
}

/// A sandbox at 10:00 IST on a Monday, RELIANCE at 1300.
fn sandbox() -> Sandbox {
    let quotes = Arc::new(StaticQuoteSource::new());
    quotes.set_ltp("RELIANCE", "NSE", Decimal::from(1300));
    let deps = SandboxDeps {
        symbols: Arc::new(StaticSymbols::new().with("RELIANCE", "NSE", 1)),
        quotes,
        clock: manual_clock_at("2026-10-05 10:00:00"),
        bus: None,
    };
    Sandbox::in_memory(deps, SandboxOptions::default()).expect("open the sandbox")
}

/// A LIMIT buy below the price, which rests.
fn resting_buy(symbol: &str, price: i64) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        quantity: 1,
        price: Some(Decimal::from(price)),
        trigger_price: None,
        price_type: "LIMIT".into(),
        product: "MIS".into(),
        strategy: "engine-test".into(),
    }
}

/// Wait until the engine has taken every command sent so far. It takes a
/// command only once it has finished with the one before, so then every
/// command but the last has been handled.
async fn until_commands_taken(sb: &Sandbox) {
    for _ in 0..1000 {
        let taken = sb
            .core()
            .engine_tx
            .read()
            .as_ref()
            .is_some_and(|tx| tx.capacity() == tx.max_capacity());
        if taken {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the engine did not take its commands");
}

/// Web test/test_gthread_sandbox_lifecycle.py::test_a_cancel_racing_a_place_is_queued_after_it
/// (398f614cc): never the symbol left subscribed. A place reads its order
/// as resting and only then sends its `Watch`; a cancel committed in
/// between (`orders::cancel`, cancel all among its callers, takes no
/// position lock) sends its `Rebuild` first. The engine handles the
/// `Rebuild`, which finds nothing needing RELIANCE, and then the place's
/// `Watch`, which must not watch it: nothing would release it until the
/// next maintenance rebuild, a minute later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_a_cancel_overtook_watches_nothing() {
    let sb = sandbox();
    // Placed and cancelled before the engine runs, so its own commands
    // went nowhere and the race below delivers them.
    let order = sb
        .place_order(resting_buy("RELIANCE", 1200))
        .await
        .expect("the order is placed")
        .orderid;
    sb.cancel_order(&order)
        .await
        .expect("the order is cancelled");
    let feed = FeedLog::new();
    sb.start_engine(feed.clone())
        .await
        .expect("the engine starts");

    // The cancel's Rebuild, then the place's Watch, then one more command,
    // so that the Watch has been handled once the engine has taken them.
    sb.core().notify_engine(EngineCmd::Rebuild);
    sb.core()
        .notify_engine(EngineCmd::Watch(SymbolKey::new("RELIANCE", "NSE")));
    sb.core().notify_engine(EngineCmd::Rebuild);
    until_commands_taken(&sb).await;

    let calls = feed.calls();
    assert!(
        calls.is_empty(),
        "RELIANCE was asked for with nothing needing it: {calls:?}"
    );
    sb.shutdown().await;
}
