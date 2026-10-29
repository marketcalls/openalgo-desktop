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
