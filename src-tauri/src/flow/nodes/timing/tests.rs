//! Delay, Wait Until, Time Window, Time Condition and the market-hours
//! gate, ported from the web's `test/test_flow_lifecycle_guards.py`
//! (70-92), `test/test_flow_qa_regressions.py` (1360-1383),
//! `test/test_flow_market_hours.py` and `test/test_gthread_review_flow.py`
//! (85-114), with the clock injected where the web read the wall clock.
//! Every time is IST whatever the machine's zone (lead decision 1): the
//! clock is set as a UTC instant, so the answers hold under TZ=UTC and
//! TZ=America/New_York alike.

use super::*;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::NodeEnv;
use crate::flow::executor::waits::workflow_wait_seconds;
use crate::flow::nodes::testkit::{answer, ctx_at, ist, logs, messages, run};
use chrono::NaiveDateTime;
use serde_json::json;
use std::sync::Arc;

fn at(wall: &str) -> NodeCtx {
    ctx_at(Arc::new(FakeFlowClient::new()), wall, NodeEnv::default())
}

#[tokio::test(start_paused = true)]
async fn delays_are_read_and_capped_as_the_web_reads_them() {
    let cases = [
        (json!({}), "Waited 1.0s", vec!["Waiting for 1.0 seconds"]),
        (
            json!({"delayMs": 1500}),
            "Waited 1.5s",
            vec!["Waiting for 1.5 seconds"],
        ),
        (
            json!({"delayValue": 1.9}),
            "Waited 1s",
            vec!["Waiting for 1 seconds"],
        ),
        (
            json!({"delayValue": "5"}),
            "Waited 5s",
            vec!["Waiting for 5 seconds"],
        ),
        (
            json!({"delayValue": 3, "delayUnit": "days"}),
            "Waited 3s",
            vec!["Waiting for 3 seconds"],
        ),
        (
            json!({"delayValue": 2, "delayUnit": "minutes"}),
            "Waited 120s",
            vec!["Waiting for 120 seconds"],
        ),
        (
            json!({"delayValue": 1, "delayUnit": "hours"}),
            "Waited 300s",
            vec![
                "Delay of 3600s exceeds the 300s maximum; waiting the maximum instead.",
                "Waiting for 300 seconds",
            ],
        ),
    ];
    for (d, waited, lines) in cases {
        let c = at("2026-10-29T10:15:00");
        let out = run(&Delay, &c, d.clone()).await;
        assert_eq!(
            answer(&out),
            json!({"status": "success", "message": waited}),
            "{}",
            d
        );
        assert_eq!(messages(&c), lines, "{}", d);
    }
    let c = at("2026-10-29T10:15:00");
    run(&Delay, &c, json!({"delayValue": 1, "delayUnit": "hours"})).await;
    assert_eq!(logs(&c)[0].0, "warning");
}

/// N-delay-02, N-logic-05: a value `int()` refuses ends the whole run.
#[tokio::test]
async fn an_unreadable_delay_ends_the_run() {
    for (d, raised) in [
        (
            json!({"delayValue": "1.5"}),
            "invalid literal for int() with base 10: '1.5'",
        ),
        (
            json!({"delayValue": "{{wait}}"}),
            "invalid literal for int() with base 10: '{{wait}}'",
        ),
        (
            json!({"delayMs": null}),
            "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'",
        ),
        (
            json!({"delayValue": [1]}),
            "int() argument must be a string, a bytes-like object or a real number, not 'list'",
        ),
    ] {
        let c = at("2026-10-29T10:15:00");
        let out = run(&Delay, &c, d.clone()).await;
        assert_eq!(out.aborted(), Some(raised), "{}", d);
    }
}

/// N-delay-05: a negative delay fails this branch only. The web's
/// `time.sleep(-3)` raised and ended the whole run (fails on the web: the
/// outcome there is an abort, not a node error).
#[tokio::test]
async fn a_negative_delay_fails_only_its_branch() {
    let c = at("2026-10-29T10:15:00");
    let out = run(&Delay, &c, json!({"delayValue": -3})).await;
    assert_eq!(out.aborted(), None);
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "sleep length must be non-negative"})
    );
    assert_eq!(messages(&c), vec!["Waiting for -3 seconds"]);
}

/// N-delay-06, N-waitUntil-04: a stopped run wakes a wait at once.
#[tokio::test]
async fn a_stopped_run_ends_a_wait() {
    let c = at("2026-10-29T10:15:00");
    c.cancel.cancel();
    let out = run(&Delay, &c, json!({"delayValue": 60})).await;
    assert_eq!(out.failure_message(), STOPPED_WHILE_WAITING);
    let out = run(&WaitUntil, &c, json!({"targetTime": "10:20"})).await;
    assert_eq!(out.failure_message(), STOPPED_WHILE_WAITING);
}

#[tokio::test(start_paused = true)]
async fn wait_until_waits_to_the_second_or_says_it_passed() {
    let cases = [
        (
            "2026-10-29T10:15:00",
            json!({"targetTime": "09:30"}),
            false,
            "Target time 09:30 already passed",
        ),
        (
            "2026-10-29T10:15:00",
            json!({}),
            false,
            "Target time 09:30 already passed",
        ),
        (
            "2026-10-29T10:15:00",
            json!({"targetTime": "10:15:00"}),
            false,
            "Target time 10:15:00 already passed",
        ),
        (
            "2026-10-29T10:15:00",
            json!({"targetTime": "{{t}}"}),
            true,
            "Waiting until 10:20 (~300s)",
        ),
        (
            "2026-10-29T10:15:00",
            json!({"targetTime": "10:20"}),
            true,
            "Waiting until 10:20 (~300s)",
        ),
        (
            "2026-10-29T10:15:00",
            json!({"targetTime": "10:15:30"}),
            true,
            "Waiting until 10:15:30 (~30s)",
        ),
        (
            "2026-10-29T23:58:00",
            json!({"targetTime": "25:99"}),
            true,
            "Waiting until 25:99 (~60s)",
        ),
        (
            "2026-10-29T09:00:00",
            json!({"targetTime": "ab:cd"}),
            true,
            "Waiting until ab:cd (~1800s)",
        ),
    ];
    for (wall, d, waited, line) in cases {
        let c = at(wall);
        c.vars.set("t", json!("10:20"));
        let before = tokio::time::Instant::now();
        let out = run(&WaitUntil, &c, d.clone()).await;
        assert_eq!(
            answer(&out),
            json!({"status": "success", "waited": waited}),
            "{}",
            d
        );
        assert_eq!(messages(&c), vec![line], "{}", d);
        if waited {
            assert!(before.elapsed() >= Duration::from_secs(30), "{}", d);
        }
    }
}

/// E-50, X-100: a wait measured in hours is refused and points at a
/// schedule trigger; a short one waits (clock injected, so neither side
/// depends on when the test runs).
#[tokio::test(start_paused = true)]
async fn a_wait_measured_in_hours_is_refused() {
    let c = at("2026-10-29T10:15:00");
    let out = run(&WaitUntil, &c, json!({"targetTime": "23:59:59"})).await;
    assert!(out.is_error());
    assert!(out.failure_message().contains("schedule trigger"));
    let c = at("2026-10-29T10:15:00");
    let out = run(&WaitUntil, &c, json!({"targetTime": "15:15"})).await;
    let message = "Wait Until 15:15 is 18000s away, over the 1800s limit. The wait holds this workflow's lock and the request that triggered it, so every trigger in between is answered 'already running'. Use a schedule trigger at 15:15 instead, or split the square-off into its own workflow.";
    assert_eq!(answer(&out), json!({"status": "error", "message": message}));
    assert_eq!(
        logs(&c),
        vec![(
            "error".to_string(),
            format!("Wait Until aborted: {}", message)
        )]
    );
    let c = at("2026-10-29T23:59:00");
    let out = run(&WaitUntil, &c, json!({"targetTime": "23:59:59"})).await;
    assert_eq!(answer(&out)["waited"], true);
}

async fn window(wall: &str, d: Value) -> (bool, String) {
    let c = at(wall);
    let out = run(&TimeWindow, &c, d).await;
    (
        out.condition_met().unwrap_or(false),
        messages(&c).pop().unwrap_or_default(),
    )
}

#[tokio::test]
async fn time_windows_follow_the_web_including_overnight() {
    let w = json!({"startTime": "09:15", "endTime": "15:30"});
    assert_eq!(
        window("2026-10-29T10:15:00", w.clone()).await,
        (
            true,
            "Time window: 09:15-15:30, in_window=True, invert=False, result=True".into()
        )
    );
    assert!(window("2026-10-29T15:30:00", w.clone()).await.0);
    assert!(!window("2026-10-29T15:30:01", w).await.0);
    assert_eq!(
        window("2026-10-29T15:31:00", json!({})).await.1,
        "Time window: 09:15-15:30, in_window=False, invert=False, result=False"
    );
    let night = json!({"startTime": "22:00", "endTime": "02:00"});
    assert!(window("2026-10-29T23:30:00", night.clone()).await.0);
    assert!(window("2026-10-29T01:59:00", night.clone()).await.0);
    assert!(!window("2026-10-29T10:15:00", night).await.0);
    let (met, line) = window(
        "2026-10-29T10:15:00",
        json!({"startTime": "09:15", "endTime": "15:30", "invertCondition": true}),
    )
    .await;
    assert!(!met);
    assert!(line.ends_with("invert=True, result=False"));
}

/// E-49, X-099: 00:00-23:59 is open all day, 22:00-02:00 wraps midnight,
/// asserted on both sides with an injected clock (the web's test read the
/// wall clock and could flake in two minutes of the day).
#[tokio::test]
async fn a_window_spanning_midnight_is_never_empty() {
    for wall in [
        "2026-10-29T00:00:00",
        "2026-10-29T12:00:00",
        "2026-10-29T23:59:00",
    ] {
        assert!(
            window(wall, json!({"startTime": "00:00", "endTime": "23:59"}))
                .await
                .0
        );
    }
    assert!(
        window(
            "2026-10-29T12:00:00",
            json!({"startTime": "00:01", "endTime": "23:58"})
        )
        .await
        .0
    );
    assert!(
        window(
            "2026-10-29T22:00:00",
            json!({"startTime": "22:00", "endTime": "02:00"})
        )
        .await
        .0
    );
    assert!(
        window(
            "2026-10-29T02:00:00",
            json!({"startTime": "22:00", "endTime": "02:00"})
        )
        .await
        .0
    );
    assert!(
        !window(
            "2026-10-29T02:01:00",
            json!({"startTime": "22:00", "endTime": "02:00"})
        )
        .await
        .0
    );
}

/// N-timeWindow-04: the text "false" does not invert the window. Fails on
/// the web, where `bool("false")` is true.
#[tokio::test]
async fn the_text_false_does_not_invert_a_window() {
    let base = json!({"startTime": "09:15", "endTime": "15:30"});
    for (flag, inverted) in [
        (json!("false"), false),
        (json!("False"), false),
        (json!("0"), false),
        (json!(""), false),
        (json!("true"), true),
        (json!("1"), true),
        (json!(true), true),
        (json!(false), false),
    ] {
        let mut d = base.clone();
        d["invertCondition"] = flag.clone();
        let (met, _) = window("2026-10-29T10:15:00", d).await;
        assert_eq!(met, !inverted, "{}", flag);
    }
}

async fn time_condition(wall: &str, d: Value) -> (Value, Vec<(String, String)>) {
    let c = at(wall);
    let out = run(&TimeCondition, &c, d).await;
    (answer(&out), logs(&c))
}

/// X-206: seconds are honoured; X-207: an unknown operator is refused.
#[tokio::test]
async fn time_conditions_compare_seconds_and_refuse_unknown_operators() {
    let (out, logs) = time_condition(
        "2026-10-29T10:15:00",
        json!({"targetTime": "10:14:40", "operator": ">="}),
    )
    .await;
    assert_eq!(out["condition"], true);
    assert_eq!(
        logs[0].1,
        "Time condition (entry): 10:15 >= 10:14:40 = True"
    );
    let (out, _) = time_condition(
        "2026-10-29T10:15:00",
        json!({"targetTime": "10:15:20", "operator": ">="}),
    )
    .await;
    assert_eq!(out["condition"], false);
    let (out, _) = time_condition("2026-10-29T10:15:00", json!({})).await;
    assert_eq!(
        out,
        json!({"status": "success", "condition": true, "condition_type": "entry",
               "current_time": "10:15:00", "target_time": "09:30", "operator": ">="})
    );
    // `==` is minute precision.
    let (out, _) = time_condition(
        "2026-10-29T10:15:20",
        json!({"targetTime": "10:15:45", "operator": "=="}),
    )
    .await;
    assert_eq!(out["condition"], true);
    let (out, _) = time_condition(
        "2026-10-29T10:15:00",
        json!({"targetTime": "10:15:01", "operator": "<", "conditionType": "exit"}),
    )
    .await;
    assert_eq!(out["condition"], true);
    assert_eq!(out["condition_type"], "exit");
    let (out, logs) = time_condition(
        "2026-10-29T10:15:00",
        json!({"targetTime": "09:30:00", "operator": "after"}),
    )
    .await;
    assert_eq!(
        out,
        json!({"status": "error", "condition": false,
               "message": "operator 'after' is not one of >=, <=, >, <, =="})
    );
    assert_eq!(
        logs[0],
        (
            "error".into(),
            "Time condition aborted: operator 'after' is not one of >=, <=, >, <, ==".into()
        )
    );
}

/// N-logic-09, decision 1: a run at 21:00 UTC is 02:30 IST, whatever the
/// machine's zone.
#[tokio::test]
async fn times_are_market_time_on_any_machine() {
    let c = ctx_at(
        Arc::new(FakeFlowClient::new()),
        "2026-10-30T02:30:00",
        NodeEnv::default(),
    );
    assert_eq!(
        ist_now(c.clock.as_ref()).format("%H:%M").to_string(),
        "02:30"
    );
    let out = run(
        &TimeCondition,
        &c,
        json!({"targetTime": "02:30", "operator": "=="}),
    )
    .await;
    assert_eq!(answer(&out)["current_time"], "02:30:00");
}

/// X-329: the wait the graph declares, as the run will sleep it.
#[test]
fn the_wait_is_read_from_the_graph_as_the_nodes_sleep() {
    let now = NaiveDateTime::parse_from_str("2026-09-23 14:45:00", "%Y-%m-%d %H:%M:%S").unwrap();
    let delay = |d: Value| json!([{"type": "delay", "data": d}]);
    let until = |t: &str| json!([{"type": "waitUntil", "data": {"targetTime": t}}]);
    assert_eq!(
        workflow_wait_seconds(&delay(json!({"delayValue": 2})), now),
        (2.0, 0.0)
    );
    assert_eq!(
        workflow_wait_seconds(
            &delay(json!({"delayValue": 2, "delayUnit": "minutes"})),
            now
        ),
        (120.0, 0.0)
    );
    assert_eq!(
        workflow_wait_seconds(&delay(json!({"delayValue": 2, "delayUnit": "hours"})), now),
        (DELAY_MAX_SECONDS as f64, 0.0)
    );
    assert_eq!(
        workflow_wait_seconds(&delay(json!({"delayMs": 1500})), now),
        (1.5, 0.0)
    );
    assert_eq!(
        workflow_wait_seconds(&delay(json!({"delayValue": "soon"})), now),
        (DELAY_MAX_SECONDS as f64, 0.0)
    );
    let two = json!([{"type": "delay", "data": {"delayValue": 4}}, {"type": "delay", "data": {"delayValue": 8}}]);
    assert_eq!(workflow_wait_seconds(&two, now), (12.0, 0.0));
    assert_eq!(workflow_wait_seconds(&until("14:45:05"), now), (0.0, 5.0));
    assert_eq!(workflow_wait_seconds(&until("15:15"), now), (0.0, 1800.0));
    assert_eq!(workflow_wait_seconds(&until("09:30"), now), (0.0, 0.0));
    assert_eq!(
        workflow_wait_seconds(&until("23:00"), now),
        (0.0, WAIT_UNTIL_MAX_SECONDS as f64)
    );
}

// --- The market-hours gate (test_flow_market_hours.py) ---------------------

/// A calendar window on 2026-08-04 (a Tuesday), as the calendar writes it.
fn session(start: &str, end: &str) -> Option<SessionWindow> {
    let ms = |hhmm: &str| ist(&format!("2026-08-04T{}:00", hhmm)).timestamp_millis();
    Some(SessionWindow {
        start_ms: ms(start),
        end_ms: ms(end),
        is_special: false,
    })
}

fn nse() -> Option<SessionWindow> {
    session("09:15", "15:30")
}

fn now(hhmm: &str, day: u32) -> chrono::DateTime<Utc> {
    ist(&format!("2026-08-{:02}T{}:00", day, hhmm))
}

fn inside(
    window: Option<SessionWindow>,
    hhmm: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> bool {
    let s = start.map(|s| json!(s));
    let e = end.map(|e| json!(e));
    within_window(window, now(hhmm, 4), s.as_ref(), e.as_ref())
}

#[test]
fn the_calendar_drives_the_window() {
    for (hhmm, want) in [
        ("09:14", false),
        ("09:15", true),
        ("15:30", true),
        ("15:31", false),
        ("03:00", false),
    ] {
        assert_eq!(inside(nse(), hhmm, None, None), want, "{}", hhmm);
    }
    let mcx = session("09:00", "23:55");
    assert!(inside(mcx, "22:30", None, None));
    assert!(!inside(nse(), "22:30", None, None));
}

#[test]
fn a_closed_day_stays_closed_whatever_the_override() {
    assert!(!within_window(None, now("12:00", 8), None, None));
    assert!(!within_window(
        None,
        now("12:00", 8),
        Some(&json!("00:00")),
        Some(&json!("23:59"))
    ));
}

#[test]
fn the_trigger_node_overrides_one_side_or_both() {
    assert!(!inside(nse(), "15:40", None, None));
    assert!(inside(nse(), "15:40", None, Some("15:40")));
    assert!(inside(nse(), "09:20", None, None));
    assert!(!inside(nse(), "09:20", Some("09:30"), None));
    assert!(!inside(nse(), "09:14", None, Some("15:40")));
    assert!(inside(nse(), "15:35", None, Some("15:40")));
    assert!(inside(nse(), "10:00", Some("10:00"), Some("14:00")));
    assert!(inside(nse(), "14:00", Some("10:00"), Some("14:00")));
    assert!(!inside(nse(), "14:01", Some("10:00"), Some("14:00")));
}

/// N-marketHours-04: garbage never widens the window to 24x7.
#[test]
fn a_malformed_override_falls_back_to_the_calendar() {
    for bad in ["", "nonsense", "25:00", "12:99", "1530", "12:", "09:15:30"] {
        assert!(!inside(nse(), "23:00", None, Some(bad)), "{}", bad);
        assert!(inside(nse(), "12:00", None, Some(bad)), "{}", bad);
    }
    assert_eq!(parse_hhmm(&json!(" 9 : 5 ")), Some(545));
    assert_eq!(parse_hhmm(&json!("+09:05")), Some(545));
    assert_eq!(parse_hhmm(&json!(915)), None);
}

#[test]
fn the_settings_are_read_from_the_trigger_node() {
    let wf = |d: Value| json!([{"type": "start", "data": d}]);
    assert_eq!(
        market_hours_config(&wf(
            json!({"marketHoursOnly": true, "marketHoursStart": "09:15",
            "marketHoursEnd": "15:40", "marketHoursExchange": "NFO"})
        )),
        MarketHours {
            enabled: true,
            start: Some(json!("09:15")),
            end: Some(json!("15:40")),
            exchange: Some(json!("NFO")),
        }
    );
    let only = market_hours_config(&wf(json!({"marketHoursOnly": true})));
    assert!(only.enabled && only.start.is_none() && only.end.is_none() && only.exchange.is_none());
    assert!(!market_hours_config(&json!([{"type": "getQuote"}])).enabled);
    assert!(!market_hours_config(&Value::Null).enabled);
    // A node saved without the keys runs ungated (N-marketHours-07).
    assert!(!market_hours_config(&wf(json!({}))).enabled);
}

/// The gate on a real calendar (the web's TestRealCalendar, and the
/// calendar's own rules, N-marketHours-05/06).
#[test]
fn the_gate_reads_the_apps_calendar() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::sqlite::migrations::run_legacy_schema(&c).unwrap();
    market_calendar::migrate(&c).unwrap();
    let nse = MarketHours {
        enabled: true,
        ..Default::default()
    };
    assert!(
        !is_within_market_hours(&c, now("12:00", 8), &nse),
        "Saturday"
    );
    assert!(
        is_within_market_hours(&c, now("12:00", 4), &nse),
        "Tuesday midday"
    );
    assert!(!is_within_market_hours(&c, now("15:40", 4), &nse));
    let extended = MarketHours {
        end: Some(json!("15:40")),
        ..nse.clone()
    };
    assert!(is_within_market_hours(&c, now("15:40", 4), &extended));
    let mcx = MarketHours {
        exchange: Some(json!("mcx")),
        ..nse.clone()
    };
    assert!(is_within_market_hours(&c, now("22:30", 4), &mcx));
    let crypto = MarketHours {
        exchange: Some(json!("CRYPTO")),
        ..nse
    };
    assert!(is_within_market_hours(&c, now("03:00", 8), &crypto));
}

/// Time fields resolve their `{{...}}` references; one that does not
/// resolve to a time fails the node, naming the field. The web read the
/// reference as a malformed time and used the default (fails on the web:
/// `{{open}}`-`{{close}}` became 09:15-15:30 and answered True at 10:15).
#[tokio::test(start_paused = true)]
async fn time_fields_resolve_their_references_or_fail() {
    let c = at("2026-10-29T10:15:00");
    c.vars.set("open", json!("11:00"));
    c.vars.set("close", json!("12:00"));
    c.vars.set("t", json!("11:00"));
    c.vars.set("bad", json!("soon"));
    let out = run(
        &TimeWindow,
        &c,
        json!({"startTime": "{{open}}", "endTime": "{{close}}"}),
    )
    .await;
    assert_eq!(out.condition_met(), Some(false));
    assert_eq!(
        messages(&c)[0],
        "Time window: 11:00-12:00, in_window=False, invert=False, result=False"
    );
    let out = run(
        &TimeCondition,
        &c,
        json!({"targetTime": "{{t}}", "operator": ">="}),
    )
    .await;
    assert_eq!(answer(&out)["condition"], false);
    assert_eq!(answer(&out)["target_time"], "11:00");
    let out = run(&TimeWindow, &c, json!({"startTime": "{{nope}}"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "condition": false,
               "message": "startTime did not resolve to a time (HH:MM or HH:MM:SS): '{{nope}}'"})
    );
    let out = run(&TimeWindow, &c, json!({"endTime": "{{bad}}"})).await;
    assert_eq!(
        out.failure_message(),
        "endTime did not resolve to a time (HH:MM or HH:MM:SS): 'soon'"
    );
    let out = run(&TimeCondition, &c, json!({"targetTime": "{{nope}}"})).await;
    assert_eq!(out.condition_met(), Some(false));
    assert!(out.is_error());
    let c = at("2026-10-29T10:15:00");
    let out = run(&WaitUntil, &c, json!({"targetTime": "{{nope}}"})).await;
    assert_eq!(
        answer(&out),
        json!({"status": "error", "message": "targetTime did not resolve to a time (HH:MM or HH:MM:SS): '{{nope}}'"})
    );
    assert_eq!(
        logs(&c)[0],
        (
            "error".into(),
            "Wait Until aborted: targetTime did not resolve to a time (HH:MM or HH:MM:SS): '{{nope}}'".into()
        )
    );
    // A built-in resolves too; a literal keeps the web's forgiving reading.
    let out = run(
        &TimeCondition,
        &at("2026-10-29T10:15:00"),
        json!({"targetTime": "{{time}}", "operator": "=="}),
    )
    .await;
    assert_eq!(answer(&out)["condition"], true);
    let out = run(
        &TimeCondition,
        &at("2026-10-29T10:15:00"),
        json!({"targetTime": "25:99"}),
    )
    .await;
    assert_eq!(answer(&out)["condition"], false);
}
