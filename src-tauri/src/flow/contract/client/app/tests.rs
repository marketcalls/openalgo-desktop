use super::*;

/// Flow is its own internal caller: not MCP, the tunnel, the Agent or
/// the limiter's overflow, and inside the never-banned block.
#[test]
fn flow_is_its_own_internal_caller() {
    use crate::agent::tools::service::AGENT_CALLER;
    use crate::server::ratelimit::OVERFLOW;
    use crate::server::source::PROXIED_CALLER;
    for other in [AGENT_CALLER, dispatch::MCP_CALLER, PROXIED_CALLER, OVERFLOW] {
        assert_ne!(FLOW_CALLER, other);
    }
    assert!(crate::db::sqlite::monitor::never_banned(
        &FLOW_CALLER.to_string()
    ));
    assert_eq!(crate::server::addr::aggregate_key(FLOW_CALLER), None);
}

#[test]
fn an_unanswered_call_may_have_taken_effect() {
    assert_eq!(
        transport_failure(Transport::Timeout)["error_type"],
        UNKNOWN_OUTCOME
    );
    assert_eq!(
        transport_failure(Transport::NoReply)["error_type"],
        UNKNOWN_OUTCOME
    );
    assert!(transport_failure(Transport::NotSent)
        .get("error_type")
        .is_none());
    let mut raw = dispatch::Raw::new(
        500,
        r#"{"status":"error","message":"Check the order book before placing it again."}"#,
    );
    raw.outcome_unknown = true;
    let v = shaped(raw);
    assert_eq!(v["error_type"], UNKNOWN_OUTCOME);
    assert_eq!(v["error"], "Check the order book before placing it again.");
}

/// A run's call goes through the real `/api/v1` handlers with the key
/// it was given: an unknown key is refused as for any client, in the
/// web client's dict.
#[tokio::test]
async fn a_call_reaches_the_api_in_process() {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    let client = AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("not-a-key"));
    let v = client.get_quotes("INFY", "NSE").await;
    assert_eq!(v["status"], "error", "{}", v);
    assert!(v["code"].as_u64().is_some_and(|c| c >= 400), "{}", v);
}

/// CL-21, CL-22: a stopped Telegram bot or an unpaired WhatsApp device
/// answers the web's sentences, and nothing is sent.
#[tokio::test]
async fn alerts_refuse_as_the_web_refuses() {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    let client = AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("k"));
    assert_eq!(
        client.telegram("hi").await,
        json!({"status": "error", "error": "Telegram bot is stopped"})
    );
    assert_eq!(
        client.whatsapp("hi", None).await,
        json!({"status": "error", "error": WHATSAPP_NOT_READY})
    );
}
/// N-telegramAlert-04: the alert's time is IST whatever the machine's
/// zone (fails under TZ=UTC or America/New_York with the host clock).
#[test]
fn telegram_alerts_are_stamped_in_market_time() {
    let now = "2026-10-30T03:45:07Z".parse().unwrap();
    assert_eq!(
        telegram_text(now, "Bought SBIN"),
        format!(
            "*Flow Alert*\n{}\nBought SBIN\n\nTime: 09:15:07",
            "\u{2500}".repeat(21)
        )
    );
}

/// An optionchain the API refuses for a field: the node reads the field
/// errors (the raw dict, which the run's errors keep and its log shows
/// as Python's str()), not "Validation error".
#[tokio::test]
async fn an_option_chain_refused_for_a_field_reports_the_field() {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    let client = AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("k"));
    let v = client.optionchain("NIFTY", "NOWHERE", "", 10).await;
    assert_eq!(v["status"], "error", "{}", v);
    assert_eq!(v["code"], 400, "{}", v);
    assert!(v["error"]["exchange"].is_array(), "{}", v);
    let outcome = crate::flow::contract::node::NodeOutcome::result(v);
    assert!(
        outcome.failure_message().starts_with("{'exchange': ["),
        "{}",
        outcome.failure_message()
    );
}

/// CL-21: a number that is not one is refused before any send, none
/// means the paired owner, and a send is answered as the web answers it.
#[test]
fn whatsapp_alerts_answer_as_the_web_answers() {
    use crate::messaging::whatsapp::Report;
    assert_eq!(
        whatsapp_targets(Some("call me")),
        Err(json!({"status": "error", "error": "Invalid WhatsApp phone number: call me"}))
    );
    assert_eq!(
        whatsapp_targets(Some("+91 98765 43210")),
        Ok(vec!["919876543210@s.whatsapp.net".to_string()])
    );
    assert_eq!(whatsapp_targets(None), Ok(vec![]));
    assert_eq!(whatsapp_targets(Some("")), Ok(vec![]));
    let sent = Report {
        sent: vec!["919876543210@s.whatsapp.net".into()],
        ..Default::default()
    };
    assert_eq!(
        whatsapp_answer(&sent),
        json!({"status": "success", "data": {"sent": ["919876543210@s.whatsapp.net"], "failed": [], "skipped": 0}})
    );
    let failed = Report {
        failed: vec![
            json!({"to": "x", "error": "not on WhatsApp"}),
            json!({"to": "y"}),
        ],
        ..Default::default()
    };
    assert_eq!(whatsapp_answer(&failed)["error"], "not on WhatsApp");
    assert_eq!(whatsapp_answer(&failed)["data"]["failed"][1]["to"], "y");
    let bare = Report {
        failed: vec![json!({"to": "x"})],
        ..Default::default()
    };
    assert_eq!(whatsapp_answer(&bare)["error"], "send failed");
}

/// CL-22: with the bot running, the Telegram alert answers each of the
/// web's refusals in turn, then queues the alert.
#[tokio::test]
async fn telegram_alerts_answer_as_the_web_answers() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    t.ctx
        .security
        .install(&crate::security::Unlocked::random().unwrap());
    let conn = t.ctx.sqlite.conn().unwrap();
    conn.execute_batch("UPDATE bot_config SET is_active = 1")
        .unwrap();
    let client = AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("k"));
    assert_eq!(
        client.telegram("hi").await,
        json!({"status": "error", "error": "Invalid API key"})
    );
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    // A new account starts with the bot off; switch it on again.
    conn.execute_batch("UPDATE bot_config SET is_active = 1")
        .unwrap();
    assert_eq!(
        client.telegram("hi").await,
        json!({"status": "error", "error": "No Telegram account linked for user: trader"})
    );
    conn.execute_batch(
        "INSERT INTO telegram_users (telegram_id, openalgo_username, notifications_enabled)
         VALUES (7, 'trader', 0)",
    )
    .unwrap();
    assert_eq!(
        client.telegram("hi").await,
        json!({"status": "error", "error": "Telegram notifications are disabled"})
    );
    let text = logs.text();
    for line in [
        "No telegram user linked for username: trader",
        "Notifications disabled for telegram user: trader",
    ] {
        assert!(text.contains(line), "{}\n{}", line, text);
    }
    conn.execute_batch("UPDATE telegram_users SET notifications_enabled = 1")
        .unwrap();
    assert_eq!(
        client.telegram("hi").await,
        json!({"status": "success", "data": {"message": "Alert queued successfully"}})
    );
    // A linked account that cannot be read is no linked account, as the
    // web's lookup answers (decision 34).
    conn.execute_batch("ALTER TABLE telegram_users RENAME TO telegram_users_gone")
        .unwrap();
    assert_eq!(
        client.telegram("hi").await,
        json!({"status": "error", "error": "No Telegram account linked for user: trader"})
    );
    assert!(
        logs.text()
            .contains("Failed to get telegram user by username: "),
        "{}",
        logs.text()
    );
}

/// CL-08: cancel-all sends the `strategy` the desktop's /api/v1
/// requires, so the API reads the body and answers on the key (403),
/// not on a missing field (400).
#[tokio::test]
async fn cancel_all_sends_the_strategy_the_api_requires() {
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    let client = AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("k"));
    let v = client.cancel_all_orders().await;
    assert_eq!(v["status"], "error", "{}", v);
    assert_eq!(v["code"], 403, "{}", v);
}
