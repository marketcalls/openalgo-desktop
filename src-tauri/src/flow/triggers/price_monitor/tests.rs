//! The price monitor's alerts against `flow_price_monitor_service.py` and
//! the web's monitor tests (`test_flow_qa_regressions.py` 156-221,
//! 1580-1644; parity T-030..T-044, X-154..X-157, X-218..X-220).

use super::*;

fn now() -> DateTime<Utc> {
    "2026-11-03T04:00:00Z".parse().unwrap()
}

fn alert(v: Value) -> Alert {
    Alert::from_node(7, v.as_object().unwrap(), now()).unwrap()
}

/// T-030, T-032, T-031 (WEB-DEFECT): the node's fields with the web's
/// defaults, the condition in its canonical word, trigger and expiration
/// lower-cased, and the channel bounds and percentage as numbers (the web
/// kept `"100"` as text and failed every poll).
#[test]
fn an_alert_is_registered_from_its_node() {
    let a = alert(
        json!({"symbol": "SBIN", "condition": "Crosses-Above", "price": "800",
        "priceLower": "100", "priceUpper": 900, "percentage": "1.5", "trigger": " Every_Time ", "expiration": "1H"}),
    );
    assert_eq!(
        (a.symbol.as_str(), a.exchange.as_str(), a.condition.as_str()),
        ("SBIN", "NSE", "crossing_up")
    );
    assert_eq!(
        (a.target, a.lower, a.upper, a.percentage),
        (800.0, Some(100.0), Some(900.0), Some(1.5))
    );
    assert_eq!(
        (a.trigger.as_str(), a.expiration.as_str()),
        ("every_time", "1h")
    );
    assert!(!a.one_shot());
    let d = alert(json!({"symbol": "SBIN"}));
    assert_eq!(
        (
            d.condition.as_str(),
            d.target,
            d.trigger.as_str(),
            d.expiration.as_str()
        ),
        ("greater_than", 0.0, "once", "none")
    );
    let bad = Alert::from_node(
        7,
        json!({"symbol": "SBIN", "priceLower": "abc"})
            .as_object()
            .unwrap(),
        now(),
    );
    assert_eq!(
        bad.unwrap_err(),
        "Price alert lower price 'abc' is not a number."
    );
}

/// T-035: every condition's formula on consecutive polls.
#[test]
fn each_condition_follows_the_monitor_formula() {
    let with = |condition: &str, last: Option<f64>| {
        let mut a = alert(
            json!({"symbol": "S", "condition": condition, "price": 100, "priceLower": 90, "priceUpper": 110, "percentage": 2}),
        );
        a.last = last;
        a
    };
    let cases: Vec<(&str, Option<f64>, f64, Option<bool>)> = vec![
        ("greater_than", None, 101.0, Some(true)),
        ("less_than", None, 101.0, Some(false)),
        ("crossing", None, 100.05, Some(true)),
        ("crossing", None, 101.0, Some(false)),
        ("crossing_up", None, 101.0, Some(true)),
        ("crossing_up", Some(101.0), 102.0, Some(false)),
        ("crossing_up", Some(99.0), 101.0, Some(true)),
        ("crossing_down", Some(101.0), 99.0, Some(true)),
        ("inside_channel", None, 95.0, Some(true)),
        ("entering_channel", None, 120.0, Some(false)),
        ("outside_channel", None, 120.0, Some(true)),
        ("exiting_channel", None, 95.0, Some(false)),
        ("moving_up", None, 101.0, Some(false)),
        ("moving_up", Some(100.0), 101.0, Some(true)),
        ("moving_down", Some(100.0), 101.0, Some(false)),
        ("moving_up_percent", Some(100.0), 102.0, Some(true)),
        ("moving_up_percent", Some(100.0), 101.0, Some(false)),
        ("moving_down_percent", Some(100.0), 97.0, Some(true)),
        ("moving_down_percent", Some(0.0), 97.0, Some(false)),
        ("near", None, 100.0, None),
    ];
    for (condition, last, ltp, want) in cases {
        assert_eq!(
            with(condition, last).evaluate(ltp),
            want,
            "{} {:?} {}",
            condition,
            last,
            ltp
        );
    }
    // A channel with no bounds uses the target for both.
    let mut a = alert(json!({"symbol": "S", "condition": "inside_channel", "price": 100}));
    assert_eq!(a.evaluate(100.0), Some(true));
    a.lower = Some(0.0);
    assert_eq!(
        a.evaluate(100.0),
        Some(true),
        "0 reads as no bound, as Python's `or`"
    );
}

/// T-037, T-038: the expiration windows from the alert's creation.
#[test]
fn an_alert_expires_after_its_window() {
    for (exp, after, want) in [
        ("1h", Duration::minutes(59), false),
        ("1h", Duration::hours(1), true),
        ("4h", Duration::hours(4), true),
        ("1d", Duration::hours(23), false),
        ("1w", Duration::days(7), true),
        ("none", Duration::days(400), false),
        ("2y", Duration::days(400), false),
    ] {
        let a = alert(json!({"symbol": "S", "expiration": exp}));
        assert_eq!(a.expired(now() + after), want, "{}", exp);
    }
}

/// T-034, T-036, X-154, X-156: a met one-shot latches before its run is
/// queued and is skipped by later polls; an every-time alert stays armed
/// and remembers the price, so a crossing needs a fresh cross.
#[test]
fn a_one_shot_latches_and_an_every_time_alert_stays_armed() {
    let alerts = Alerts::default();
    let once = alerts.add(alert(
        json!({"symbol": "S", "condition": "above", "price": 100}),
    ));
    assert_eq!(alerts.observe(7, once, 99.0), Observed::Quiet);
    assert_eq!(alerts.get(7).unwrap().last, Some(99.0));
    assert!(matches!(alerts.observe(7, once, 101.0), Observed::Fire(_)));
    assert!(alerts.get(7).unwrap().triggered);
    assert!(
        alerts.to_check().is_empty(),
        "a latched one-shot is not polled"
    );

    let every = alerts.add(alert(
        json!({"symbol": "S", "condition": "crosses_above", "price": 100, "trigger": "every_time"}),
    ));
    assert_ne!(every, once);
    assert_eq!(alerts.observe(7, every, 99.0), Observed::Quiet);
    assert!(matches!(alerts.observe(7, every, 101.0), Observed::Fire(_)));
    let a = alerts.get(7).unwrap();
    assert!(!a.triggered && a.last == Some(101.0));
    assert_eq!(
        alerts.observe(7, every, 102.0),
        Observed::Quiet,
        "no fresh cross"
    );
    assert_eq!(alerts.to_check().len(), 1);
}

/// X-157, X-218, X-220, T-039, T-041, T-042: a run claims its alert by
/// registration, never by workflow; a replaced alert is left alone; an
/// alert whose run did not execute is re-armed; one run at a time.
#[test]
fn runs_act_only_on_the_alert_that_queued_them() {
    let alerts = Alerts::default();
    let old = alerts.add(alert(json!({"symbol": "S", "price": 1})));
    assert!(matches!(alerts.observe(7, old, 2.0), Observed::Fire(_)));
    assert!(alerts.claim_pending(7));
    assert!(!alerts.claim_pending(7), "one run queued at a time");
    // The trader deactivated and reactivated meanwhile.
    let new = alerts.add(alert(json!({"symbol": "S", "price": 1})));
    assert!(!alerts.is_current(7, old));
    assert!(!alerts.retire(7, old), "the new registration is left alone");
    assert!(alerts.is_current(7, new));
    assert_eq!(alerts.observe(7, old, 2.0), Observed::Gone);
    alerts.release_pending(7);
    // Already running: the latch comes off, the alert stays.
    assert!(matches!(alerts.observe(7, new, 2.0), Observed::Fire(_)));
    alerts.rearm(7, new);
    assert!(!alerts.get(7).unwrap().triggered);
    // A run that executed (even one the broker rejected) retires it.
    assert!(alerts.retire(7, new));
    assert!(alerts.get(7).is_none());
}

/// T-044: the status rows, canonical condition.
#[test]
fn the_status_lists_every_alert() {
    let alerts = Alerts::default();
    alerts.add(alert(
        json!({"symbol": "SBIN", "condition": "below", "price": 750}),
    ));
    assert_eq!(
        alerts.status(),
        vec![
            json!({"workflow_id": 7, "symbol": "SBIN", "exchange": "NSE", "condition": "less_than",
            "target_price": 750.0, "last_price": null, "triggered": false})
        ]
    );
    assert_eq!(alerts.len(), 1);
    alerts.clear();
    assert!(alerts.is_empty());
}
