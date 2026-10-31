//! The price monitor's server log lines, each in the web's words
//! (`flow_price_monitor_service.py`), the edges of its formulas and
//! expiration windows, and the feed release lines (parity T-030, T-033,
//! T-034, T-035, T-037, T-039, T-041, R-28, N-stream-04).

use super::support::*;
use super::*;
use crate::flow::triggers::price_monitor::{Alert, Alerts, Observed};
use chrono::Duration;

/// A side of the test's own, with the app's start-up trigger side stopped
/// first: its restore would hold the same alerts and write the same lines.
async fn own_side(t: &crate::state::testing::TestCtx) -> Arc<TriggerSide> {
    t.ctx.flow.triggers().stop().await;
    side(t)
}

/// An alert of workflow 41 registered at the tests' fixed instant.
fn alert(v: Value) -> Alert {
    Alert::from_node(41, &data(v), "2026-11-03T04:00:00Z".parse().unwrap()).unwrap()
}

/// `id`'s alert, fired by a tick at 812 (`above 800`, once).
fn fired(s: &TriggerSide, id: i64, now: chrono::DateTime<chrono::Utc>) -> (u64, Alert) {
    let once = json!({"symbol": "SBIN", "condition": "above", "price": 800});
    let gen = s
        .alerts
        .add(Alert::from_node(id, &data(once), now).unwrap());
    let Observed::Fire(a) = s.alerts.observe(id, gen, 812.0) else {
        panic!("the alert should fire at 812");
    };
    (gen, *a)
}

fn runs_of(app: &Arc<AppState>, id: i64) -> Vec<crate::flow::contract::model::Execution> {
    crate::flow::store::executions::list(&app.sqlite.conn().unwrap(), id, 10).unwrap()
}

/// Wait (bounded, past one 5 s poll interval) until `done` holds.
async fn wait_past_a_poll(logs: &Logged, what: &str, done: impl Fn(&Logged) -> bool) {
    for _ in 0..1000 {
        if done(logs) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("never {}: {:?}", what, logs.0.lock());
}

/// T-030, R-28: an added alert is logged as the web logs it, with its
/// canonical condition and the target as a float; a node with no symbol
/// (a PUT re-registers what was saved) registers the web's default "".
#[tokio::test]
async fn an_added_alert_is_logged_in_the_webs_words() {
    let t = app();
    let s = own_side(&t).await;
    let (_guard, logs) = logged();
    s.register(
        41,
        "priceAlert",
        &data(json!({"symbol": "SBIN", "condition": "above", "price": "800"})),
    )
    .await
    .unwrap();
    assert!(
        logs.has("Added price alert for workflow 41: SBIN@NSE greater_than 800.0"),
        "{:?}",
        logs.0.lock()
    );
    s.register(41, "priceAlert", &data(json!({"exchange": "NFO"})))
        .await
        .unwrap();
    let a = s.alerts.get(41).unwrap();
    assert_eq!((a.symbol.as_str(), a.target), ("", 0.0));
    assert!(logs.has("Added price alert for workflow 41: @NFO greater_than 0.0"));
}

/// T-033 (decision 32): the poll keeps watching while an alert is left;
/// when the last one is removed it writes the web's stopped line, once
/// (the stop that follows writes nothing more).
#[tokio::test]
async fn the_monitor_stops_when_its_last_alert_is_removed() {
    let t = app();
    let s = own_side(&t).await;
    let (_guard, logs) = logged();
    s.start_drivers();
    let watch = |id: i64| data(json!({"symbol": "SBIN", "price": 800 + id}));
    s.register(41, "priceAlert", &watch(41)).await.unwrap();
    // The first poll checked 41 alone (this app has no API key, so it
    // warns per alert) and sleeps; 42 is added and 41 removed meanwhile.
    wait_for(&logs, "No API key for alert workflow 41").await;
    s.register(42, "priceAlert", &watch(42)).await.unwrap();
    s.unregister(41).await.unwrap();
    wait_past_a_poll(&logs, "polled 42", |l| {
        l.has("No API key for alert workflow 42")
    })
    .await;
    assert!(!logs.has("Price monitoring stopped"), "one alert is left");
    assert!(logs.has("Price monitoring started with 1 alerts"));
    s.unregister(42).await.unwrap();
    assert!(logs.has("Removed price alert for workflow 42"));
    wait_past_a_poll(&logs, "stopped", |l| l.has("Price monitoring stopped")).await;
    TriggerHooks::stop(s.as_ref()).await;
    assert_eq!(logs.starting_with("Price monitoring stopped").len(), 1);
    assert_eq!(logs.starting_with("Price monitoring started").len(), 1);
}

/// T-034: a quote the API refuses is the web's debug line with the
/// client's dict as Python prints it; an alert past its window says it
/// expired and is not quoted.
#[tokio::test]
async fn the_poll_logs_a_failed_quote_and_an_expiry() {
    let t = keyed();
    let s = own_side(&t).await;
    let id = save(&t.ctx, node("priceAlert", json!({"symbol": "SBIN"})), true);
    s.register(
        id,
        "priceAlert",
        &data(json!({"symbol": "SBIN", "price": 800, "expiration": "4h"})),
    )
    .await
    .unwrap();
    let (_guard, logs) = logged();
    assert!(!s.check_prices(false).await);
    let failed = logs.starting_with("Failed to get quote for SBIN: {");
    assert_eq!(failed.len(), 1, "{:?}", logs.0.lock());
    assert!(
        failed[0].contains("'status': 'error'") && failed[0].ends_with('}'),
        "{}",
        failed[0]
    );
    t.clock.set("2026-11-03T08:00:00Z".parse().unwrap());
    s.check_prices(false).await;
    assert!(logs.has(&format!(
        "Price alert for workflow {} expired after 4h; no longer watching.",
        id
    )));
    assert_eq!(
        logs.starting_with("Failed to get quote").len(),
        1,
        "an expired alert is not quoted"
    );
    assert!(s.alerts.get(id).is_none());
}

/// T-034, T-035: a met condition writes the web's triggered line (its
/// canonical condition, the price and target as floats); a condition no
/// formula knows writes the web's error with its canonical word on every
/// poll and never fires.
#[test]
fn a_fire_and_an_unknown_condition_are_logged_as_the_web_does() {
    let alerts = Alerts::default();
    let (_guard, logs) = logged();
    let gen = alerts.add(alert(
        json!({"symbol": "SBIN", "condition": "Crosses-Below", "price": 800}),
    ));
    assert!(matches!(alerts.observe(41, gen, 799.5), Observed::Fire(_)));
    assert!(logs.has(
        "Price alert triggered for workflow 41: SBIN@NSE crossing_down (price: 799.5, target: 800.0)"
    ));
    let gen = alerts.add(alert(
        json!({"symbol": "SBIN", "condition": "Near-By", "price": 800}),
    ));
    for ltp in [799.0, 801.0] {
        assert_eq!(alerts.observe(41, gen, ltp), Observed::Quiet);
    }
    let line =
        "Price alert for workflow 41 has an unrecognized condition 'near_by'; it can never trigger.";
    assert!(logs.has(line), "{:?}", logs.0.lock());
    assert_eq!(logs.starting_with(line).len(), 2, "once per poll");
}

/// T-035: each formula at its edges: the price equal to the target or a
/// bound, equal to the previous price, a crossing down with no previous
/// price, a real move down, a percentage move at its threshold; and the
/// crossing band, the LTP's 0.1 % (the monitor's, not the node's
/// level-based one), inclusive.
#[test]
fn each_condition_holds_at_its_edges() {
    let at = |condition: &str, last: Option<f64>, ltp: f64| {
        let mut a = alert(json!({"symbol": "S", "condition": condition, "price": 100,
            "priceLower": 90, "priceUpper": 110, "percentage": 2}));
        a.last = last;
        a.evaluate(ltp)
    };
    let cases: Vec<(&str, Option<f64>, f64, bool)> = vec![
        ("greater_than", None, 100.0, false),
        ("greater_than", None, 100.01, true),
        ("less_than", None, 100.0, false),
        ("less_than", None, 99.99, true),
        ("crossing_up", None, 100.0, false),
        ("crossing_up", Some(100.0), 100.5, true),
        ("crossing_up", Some(100.0), 100.0, false),
        ("crossing_up", Some(100.5), 101.0, false),
        ("crossing_down", None, 99.5, true),
        ("crossing_down", None, 100.0, false),
        ("crossing_down", Some(100.0), 99.5, true),
        ("crossing_down", Some(100.0), 100.0, false),
        ("crossing_down", Some(99.5), 99.0, false),
        ("inside_channel", None, 90.0, true),
        ("inside_channel", None, 110.0, true),
        ("entering_channel", None, 89.99, false),
        ("entering_channel", None, 110.01, false),
        ("outside_channel", None, 90.0, false),
        ("outside_channel", None, 110.0, false),
        ("exiting_channel", None, 89.99, true),
        ("exiting_channel", None, 110.01, true),
        ("moving_down", Some(100.0), 99.0, true),
        ("moving_down", Some(100.0), 100.0, false),
        ("moving_down", None, 99.0, false),
        ("moving_up", Some(100.0), 100.0, false),
        ("moving_up_percent", Some(100.0), 102.0, true),
        ("moving_up_percent", Some(100.0), 101.99, false),
        ("moving_down_percent", Some(100.0), 98.0, true),
        ("moving_down_percent", Some(100.0), 98.01, false),
    ];
    for (condition, last, ltp, want) in cases {
        assert_eq!(
            at(condition, last, ltp),
            Some(want),
            "{} {:?} {}",
            condition,
            last,
            ltp
        );
    }
    let crossing = |target: f64| {
        alert(json!({"symbol": "S", "condition": "crossing", "price": target})).evaluate(1000.0)
    };
    for (target, want) in [
        (999.0, true),
        (1001.0, true),
        (998.999, false),
        (1001.0005, false),
    ] {
        assert_eq!(crossing(target), Some(want), "target {} at 1000", target);
    }
}

/// T-037: each window ends exactly at its length from the alert's
/// creation: not a second before, and at it.
#[test]
fn each_expiration_window_ends_at_its_length() {
    for (expiration, window) in [
        ("1h", Duration::hours(1)),
        ("4h", Duration::hours(4)),
        ("1d", Duration::days(1)),
        ("1w", Duration::weeks(1)),
    ] {
        let a = alert(json!({"symbol": "S", "expiration": expiration}));
        assert!(
            !a.expired(a.created + window - Duration::seconds(1)),
            "{} a second before its end",
            expiration
        );
        assert!(a.expired(a.created + window), "{} at its end", expiration);
    }
}

/// T-039, T-041: a second fire while the first run is queued is skipped
/// with the web's debug line; the queued run, its alert replaced meanwhile,
/// is dropped with the web's line and its one-shot re-armed (its run did
/// not execute), while the new alert, latched by its own tick, stays so.
#[tokio::test]
async fn a_queued_run_for_a_replaced_alert_says_why_it_was_dropped() {
    let t = app();
    let s = own_side(&t).await;
    let id = save(&t.ctx, node("priceAlert", json!({"symbol": "SBIN"})), true);
    let (_, alert) = fired(&s, id, t.ctx.now());
    let (_guard, logs) = logged();
    let busy = s.price_runs.clone().acquire_many_owned(4).await.unwrap();
    s.queue_price_run(alert.clone(), 812.0);
    s.queue_price_run(alert, 812.0);
    assert!(logs.has(&format!(
        "Price alert for workflow {} already has a run queued; skipping this tick.",
        id
    )));
    // The new alert latched too: the old run's re-arm must leave it so.
    let (newer, _) = fired(&s, id, t.ctx.now());
    drop(busy);
    wait_for(
        &logs,
        &format!(
            "Dropping queued price-alert run for workflow {}: the alert was removed or replaced before this run claimed it.",
            id
        ),
    )
    .await;
    wait_for(
        &logs,
        &format!(
            "Re-arming one-shot price alert for workflow {}: the run did not execute.",
            id
        ),
    )
    .await;
    let current = s.alerts.get(id).unwrap();
    assert_eq!((current.gen, current.triggered), (newer, true));
    assert!(runs_of(&t.ctx, id).is_empty());
}

/// T-039, T-041: a queued run whose workflow is no longer active is
/// dropped with the web's line; a read of the active flag that fails is
/// logged and counts as inactive (fail closed). Neither runs, and each
/// one-shot is re-armed and still watched.
#[tokio::test]
async fn a_queued_run_for_an_inactive_or_unreadable_workflow_runs_nothing() {
    let t = keyed();
    let s = own_side(&t).await;
    let graph = || node("priceAlert", json!({"symbol": "SBIN"}));
    let inactive = save(&t.ctx, graph(), false);
    let unreadable = save(&t.ctx, graph(), true);
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE flow_workflows SET is_active = 'maybe' WHERE id = ?1",
            [unreadable],
        )
        .unwrap();
    let (_guard, logs) = logged();
    for id in [inactive, unreadable] {
        let (gen, alert) = fired(&s, id, t.ctx.now());
        s.queue_price_run(alert, 812.0);
        wait_for(
            &logs,
            &format!(
                "Dropping queued price-alert run for workflow {}: the workflow is no longer active.",
                id
            ),
        )
        .await;
        wait_for(
            &logs,
            &format!(
                "Re-arming one-shot price alert for workflow {}: the run did not execute.",
                id
            ),
        )
        .await;
        let a = s.alerts.get(id).unwrap();
        assert!(a.gen == gen && !a.triggered, "re-armed and still watched");
        assert!(runs_of(&t.ctx, id).is_empty(), "workflow {} ran", id);
    }
    let unread = format!("Could not confirm workflow {} is active: ", unreadable);
    assert_eq!(logs.starting_with(&unread).len(), 1, "{:?}", logs.0.lock());
    assert!(logs
        .starting_with(&format!("Could not confirm workflow {} ", inactive))
        .is_empty());
}

/// N-stream-04 (decision 36): a release writes the web's lines: each
/// subscription the feed did not give back, then how many of the
/// workflow's record were released; a workflow holding none writes
/// nothing.
#[tokio::test]
async fn a_feed_release_logs_the_webs_lines() {
    let subs = crate::flow::contract::subscriptions::Subscriptions::default();
    subs.record(Some(5), "PASS", "NSE", "LTP");
    subs.record(Some(5), "FAIL", "BSE", "Quote");
    let f = feed(false);
    let (_guard, logs) = logged();
    assert_eq!(subs.release(5, &f).await, 1);
    assert!(
        logs.has("Failed to release Quote on BSE:FAIL"),
        "{:?}",
        logs.0.lock()
    );
    assert!(logs.has("Released 1/2 subscription(s) for workflow 5"));
    f.refuse.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(subs.release(5, &f).await, 0);
    assert!(logs.has("Released 0/1 subscription(s) for workflow 5"));
    assert_eq!(subs.release(6, &f).await, 0);
    assert_eq!(logs.starting_with("Released ").len(), 2);
    assert_eq!(logs.starting_with("Failed to release ").len(), 2);
}
