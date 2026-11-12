//! Telegram and WhatsApp alerts, ported from the web's
//! `test/test_flow_qa_regressions.py` (85-152) and
//! `test/test_gthread_messaging_whatsapp.py` (602-610).

use super::*;
use crate::flow::contract::client::fake::SentAlert;
use crate::flow::nodes::testkit::{answer, ctx, logs, run};

/// X-153: a legacy `username` is ignored; the client is asked once, with
/// the message only (the owner of the API key is its recipient).
#[tokio::test]
async fn telegram_goes_to_the_owner_whatever_the_node_names() {
    let (client, c) = ctx();
    c.vars.set("sym", json!("SBIN"));
    let out = run(
        &TelegramAlert,
        &c,
        json!({"message": "Owner-only notice {{sym}}", "username": "other_user"}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "data": {"message": "Alert queued successfully"}})
    );
    assert_eq!(
        client.alerts(),
        vec![SentAlert {
            channel: "telegram",
            message: "Owner-only notice SBIN".into(),
            to: None,
        }]
    );
    assert_eq!(
        logs(&c),
        vec![
            ("info".into(), "Sending Telegram alert: Owner-only notice SBIN".into()),
            (
                "info".into(),
                "Telegram result: {'data': {'message': 'Alert queued successfully'}, 'status': 'success'}".into()
            ),
        ]
    );
}

/// N-telegramAlert-05: a failed send is the node's failure.
#[tokio::test]
async fn a_failed_alert_fails_the_node() {
    let (client, c) = ctx();
    client.alert_reply(json!({"status": "error", "error": "Telegram bot is stopped"}));
    let out = run(&TelegramAlert, &c, json!({"message": "x"})).await;
    assert!(out.is_error());
    assert_eq!(out.failure_message(), "Telegram bot is stopped");
    assert_eq!(logs(&c)[1].0, "error");
}

/// N-whatsappAlert-01..05, X-342: the number is interpolated, empty means
/// the paired device itself, and the service's own refusal is the node's.
#[tokio::test]
async fn whatsapp_sends_to_the_number_or_to_self() {
    let (client, c) = ctx();
    c.vars.set("phone", json!("+91 98765 43210"));
    run(
        &WhatsappAlert,
        &c,
        json!({"to": "{{phone}}", "message": "Filled"}),
    )
    .await;
    run(&WhatsappAlert, &c, json!({"message": "Self"})).await;
    run(
        &WhatsappAlert,
        &c,
        json!({"to": "{{nobody}}", "message": "Raw"}),
    )
    .await;
    let alerts = client.alerts();
    assert_eq!(alerts[0].to.as_deref(), Some("+91 98765 43210"));
    assert_eq!(alerts[1].to, None);
    assert_eq!(alerts[2].to.as_deref(), Some("{{nobody}}"));
    let lines: Vec<String> = logs(&c).into_iter().map(|(_, m)| m).collect();
    assert_eq!(
        lines[0],
        "Sending WhatsApp alert to +91 98765 43210: Filled"
    );
    assert_eq!(lines[2], "Sending WhatsApp alert to self: Self");
    let (client, c) = ctx();
    let logged_out = "WhatsApp is logged out. Pair the device again from /whatsapp.";
    client.alert_reply(json!({"status": "error", "error": logged_out}));
    let out = run(&WhatsappAlert, &c, json!({"message": "09:15 alert"})).await;
    assert_eq!(out.failure_message(), logged_out);
    assert_eq!(logs(&c)[1].0, "error");
}

/// Web test/test_alerts_flow_mcp.py::test_the_executor_dispatches_the_node_with_its_variables_filled_in
#[tokio::test]
async fn slack_and_discord_post_the_message_with_its_variables_filled_in() {
    let (client, c) = ctx();
    c.vars.set("sym", json!("NIFTY"));
    for node in [&SlackAlert as &dyn FlowNode, &DiscordAlert] {
        let out = run(node, &c, json!({"message": "Bought {{sym}}"})).await;
        assert!(!out.is_error());
    }
    let sent: Vec<(&str, String)> = client
        .alerts()
        .into_iter()
        .map(|a| (a.channel, a.message))
        .collect();
    assert_eq!(
        sent,
        vec![
            ("slack", "Bought NIFTY".to_string()),
            ("discord", "Bought NIFTY".to_string())
        ]
    );
    assert_eq!(
        logs(&c)[0],
        ("info".into(), "Sending Slack alert: Bought NIFTY".into())
    );
    assert_eq!(
        logs(&c)[3].1,
        "Discord result: {'data': {'message': 'Alert queued successfully'}, 'status': 'success'}"
    );
}

/// Web test/test_alerts_flow_mcp.py::test_the_run_log_keeps_a_preview_not_the_whole_message:
/// every run's log is stored, and one variable can make a message as long
/// as the channel allows.
#[tokio::test]
async fn the_run_log_keeps_a_preview_not_the_whole_message() {
    for node in [&SlackAlert as &dyn FlowNode, &DiscordAlert] {
        let (client, c) = ctx();
        let message = "z".repeat(5000);
        run(node, &c, json!({"message": message})).await;
        assert_eq!(
            client.alerts()[0].message,
            message,
            "the channel gets it all"
        );
        let lines = logs(&c);
        assert!(lines.iter().all(|(_, m)| m.len() < 400), "{:?}", lines);
        assert!(
            lines[0].1.ends_with("... (5000 characters)"),
            "{}",
            lines[0].1
        );
    }
}

/// A refused post is the node's failure, as for Telegram.
#[tokio::test]
async fn a_refused_post_fails_the_node() {
    let (client, c) = ctx();
    let off = "Slack alerts are turned off. Turn them on from the Alerts page in OpenAlgo.";
    client.alert_reply(json!({"status": "error", "error": off}));
    let out = run(&SlackAlert, &c, json!({"message": "x"})).await;
    assert!(out.is_error());
    assert_eq!(out.failure_message(), off);
    assert_eq!(logs(&c)[1].0, "error");
}
