//! An order socket's update is completed by the broker before it is
//! published (bk-b RB-11), within a deadline: as it came by default, and
//! when the broker does not answer in time.

use super::{completed, ORDER_COMPLETION_DEADLINE};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::mock::MockBroker;

fn update(orderid: &str, symbol: &str) -> OrderUpdate {
    OrderUpdate {
        orderid: orderid.into(),
        symbol: symbol.into(),
        order_status: "complete".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn an_update_is_published_as_the_broker_completed_it() {
    let broker = MockBroker::new("mock");
    assert_eq!(completed(&broker, update("1", "")).await, update("1", ""));
    broker
        .order_completions
        .lock()
        .insert("2".into(), update("2", "SBIN"));
    assert_eq!(
        completed(&broker, update("2", "")).await,
        update("2", "SBIN")
    );
}

#[tokio::test(start_paused = true)]
async fn an_update_the_broker_does_not_complete_in_time_goes_as_it_came() {
    let broker = MockBroker::new("mock");
    broker
        .order_completions
        .lock()
        .insert("3".into(), update("3", "SBIN"));
    *broker.completion_delay.lock() = Some(ORDER_COMPLETION_DEADLINE * 2);
    let started = tokio::time::Instant::now();
    assert_eq!(completed(&broker, update("3", "")).await, update("3", ""));
    assert_eq!(started.elapsed(), ORDER_COMPLETION_DEADLINE);
}
