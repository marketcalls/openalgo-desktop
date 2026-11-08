//! The bridge's waiters and retries (audit C01, C02).

use super::super::BrokerBridge;
use crate::brokers::common::symbols::SymbolResolver;
use crate::feed::source::{InstrumentKey, MarketDataSource, Mode};
use crate::websocket::WebSocketManager;
use futures_util::FutureExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

fn bridge() -> Arc<BrokerBridge> {
    BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new())
}

fn sbin() -> InstrumentKey {
    InstrumentKey::new("SBIN", "NSE")
}

/// A retry is for a reference still held that nobody wants: one for a key
/// the managers forgot (a new broker session) or that is wanted again is
/// dropped, so the reconcile loop is not woken for nothing.
#[tokio::test]
async fn a_retry_for_nothing_held_is_dropped() {
    let b = bridge();
    let k = (sbin(), Mode::Ltp, 5);
    b.state
        .lock()
        .ops
        .retry
        .insert(k.clone(), (Instant::now(), 0));
    assert!(b.state.lock().ops.next_retry().is_some());
    b.plan();
    assert_eq!(b.state.lock().ops.next_retry(), None, "nothing is held");

    b.state
        .lock()
        .ops
        .retry
        .insert(k, (Instant::now() + Duration::from_secs(5), 1));
    b.reset();
    assert_eq!(b.state.lock().ops.next_retry(), None, "a new session");
}

/// A key the managers hold nothing for is released at once, and one
/// subscribed again before its release started answers that release:
/// another owner keeps the key.
#[tokio::test]
async fn a_release_of_nothing_held_and_a_key_wanted_again_answer_at_once() {
    let b = bridge();
    let released = b.unsubscribe(&sbin(), Mode::Quote, 5).now_or_never();
    assert_eq!(released, Some(Ok(())));

    let k = (sbin(), Mode::Quote, 5);
    let waiter = {
        let mut st = b.state.lock();
        st.ops.wait_release(k.clone())
    };
    drop(b.subscribe(&sbin(), Mode::Quote, 5));
    assert_eq!(waiter.now_or_never(), Some(Ok(Ok(()))));
    let ops = b.operations();
    assert_eq!((ops.waiting, ops.releasing), (1, 0));
}
