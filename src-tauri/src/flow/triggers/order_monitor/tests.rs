//! The order-update watches against `flow_order_update_monitor_service.py`
//! (parity T-051..T-060, X-216).

use super::*;

fn watch(v: Value) -> Result<Watch, String> {
    Watch::from_node(5, v.as_object().unwrap())
}

fn update(orderid: &str, symbol: &str, status: &str) -> OrderUpdate {
    OrderUpdate {
        orderid: orderid.into(),
        symbol: symbol.into(),
        exchange: "NSE".into(),
        order_status: status.into(),
        filled_quantity: 10,
        average_price: 812.5,
        ..Default::default()
    }
}

/// T-051: the web's refusals, word for word, and the defaults.
#[test]
fn a_watch_needs_a_literal_order_id_or_a_symbol_and_a_known_status() {
    assert_eq!(
        watch(json!({})).unwrap_err(),
        "orderUpdateTrigger needs an Order ID or a Symbol to watch"
    );
    assert!(watch(json!({"orderId": "{{webhook.orderid}}"}))
        .unwrap_err()
        .starts_with(
            "orderUpdateTrigger Order ID must be a literal order id, not a {{variable}} reference"
        ));
    assert_eq!(
        watch(json!({"symbol": "SBIN", "status": "filled"})).unwrap_err(),
        "orderUpdateTrigger status must be one of ['any', 'cancelled', 'complete', 'open', 'rejected', 'trigger pending'], got 'filled'"
    );
    let w = watch(json!({"symbol": "SBIN", "exchange": "", "status": "Trigger_Pending"})).unwrap();
    assert_eq!(w.status, "trigger pending");
    assert_eq!(w.exchange, None, "an empty exchange is no filter");
    assert_eq!(w.trigger, "once");
    let d = watch(json!({"orderId": "2511030001"})).unwrap();
    assert_eq!((d.status.as_str(), d.symbol.as_deref()), ("complete", None));
}

/// T-053: status (or any), order id, symbol and exchange, each exact.
#[test]
fn an_update_matches_every_filter_it_names() {
    let w = watch(json!({"symbol": "SBIN", "exchange": "NSE", "status": "complete"})).unwrap();
    assert!(w.matches(&update("1", "SBIN", "COMPLETE")));
    assert!(!w.matches(&update("1", "SBIN", "rejected")));
    assert!(
        !w.matches(&update("1", "sbin", "complete")),
        "case-sensitive symbol"
    );
    let mut other = update("1", "SBIN", "complete");
    other.exchange = String::new();
    assert!(
        !w.matches(&other),
        "a named exchange is not met by a missing one"
    );
    let any = watch(json!({"orderId": "42", "status": "any"})).unwrap();
    assert!(any.matches(&update("42", "X", "open")));
    assert!(!any.matches(&update("43", "X", "open")));
}

/// T-054, T-056: a one-time watch is claimed once, however many updates
/// arrive; retired after a run that executed, re-armed after one that did
/// not.
#[test]
fn a_one_time_watch_fires_once() {
    let ws = Watches::default();
    ws.attach();
    let gen = ws.add(watch(json!({"symbol": "SBIN"})).unwrap());
    let u = update("1", "SBIN", "complete");
    assert_eq!(ws.claim(&u), vec![(5, gen)]);
    assert!(ws.claim(&u).is_empty(), "claimed: no second run");
    assert!(matches!(ws.after_run(5, gen, false), After::Rearmed));
    assert_eq!(ws.claim(&u), vec![(5, gen)]);
    assert!(matches!(ws.after_run(5, gen, true), After::Retire));
    assert!(ws.get(5).is_none());
}

/// T-057 (WEB-DEFECT): an every-time watch whose run is going keeps the
/// newest update and runs once more for it (the web dropped it silently).
#[test]
fn an_every_time_watch_runs_again_for_an_update_it_received_meanwhile() {
    let ws = Watches::default();
    ws.attach();
    let gen =
        ws.add(watch(json!({"symbol": "SBIN", "status": "any", "trigger": "every_time"})).unwrap());
    assert_eq!(ws.claim(&update("1", "SBIN", "open")).len(), 1);
    assert!(ws.claim(&update("2", "SBIN", "open")).is_empty());
    assert!(ws.claim(&update("3", "SBIN", "complete")).is_empty());
    match ws.after_run(5, gen, true) {
        After::Again(u) => assert_eq!(
            (u.orderid.as_str(), u.order_status.as_str()),
            ("3", "complete")
        ),
        other => panic!("{:?}", other),
    }
    assert!(matches!(ws.after_run(5, gen, true), After::Done));
    assert_eq!(
        ws.claim(&update("4", "SBIN", "open")).len(),
        1,
        "free again"
    );
}

/// A run for a replaced watch leaves the new one alone.
#[test]
fn a_replaced_watch_is_left_alone() {
    let ws = Watches::default();
    ws.attach();
    let old = ws.add(watch(json!({"symbol": "SBIN"})).unwrap());
    ws.claim(&update("1", "SBIN", "complete"));
    let new = ws.add(watch(json!({"symbol": "SBIN"})).unwrap());
    assert!(!ws.is_current(5, old));
    assert!(matches!(ws.after_run(5, old, true), After::Done));
    assert!(ws.is_current(5, new));
}

/// T-059, T-060, X-216: the status rows; a shutdown forgets the watches
/// and ignores updates until the monitor is attached again.
#[test]
fn shutdown_detaches_and_a_restart_attaches_again() {
    let ws = Watches::default();
    ws.attach();
    ws.add(watch(json!({"orderId": "42", "status": "rejected"})).unwrap());
    assert_eq!(
        ws.status(),
        json!({"watches_count": 1, "watches": [{"workflow_id": 5, "order_id": "42", "symbol": null,
            "exchange": null, "status": "rejected", "triggered": false}]})
    );
    ws.detach();
    assert_eq!(ws.status()["watches_count"], 0);
    let gen = ws.add(watch(json!({"symbol": "SBIN"})).unwrap());
    assert!(
        ws.claim(&update("1", "SBIN", "complete")).is_empty(),
        "detached"
    );
    ws.attach();
    assert_eq!(ws.claim(&update("1", "SBIN", "complete")), vec![(5, gen)]);
}
