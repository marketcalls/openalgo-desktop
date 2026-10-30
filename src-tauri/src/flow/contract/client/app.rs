//! The in-app [`FlowClient`]: the `/api/v1` handler stack in process, as the
//! Agent and MCP reach it (`mcp::dispatch::run_as`), with the trader's API
//! key, so a node gets the same validation, analyzer routing, Semi-Auto
//! queue, events and reply shapes as any API client, and never a socket.
//!
//! Calls carry [`FLOW_CALLER`], Flow's own address in the app's internal
//! block, so Flow has its own share of the `/api/v1` rate limit and never
//! spends an SDK client's, MCP's or the Agent's.
//!
//! Alerts go through the desktop's native Telegram and WhatsApp services
//! with the web client's checks and sentences.

use super::{handle_response, ApiCall, FlowClient, UNKNOWN_OUTCOME};
use crate::mcp::dispatch::{self, Transport};
use crate::security::Secret;
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Weak;
use std::time::Duration;

/// The address Flow's in-process calls carry: its own identity in the
/// discard-only `100::/64` block, beside MCP (`100::1`), the tunnel
/// (`100::2`) and the Agent (`100::3`).
pub const FLOW_CALLER: IpAddr = IpAddr::V6(Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 4));

/// How long a node waits for one call. A slow broker never cancels an
/// order mid-flight: the call finishes in a task the app owns, and the node
/// is told the outcome is unknown.
pub const CALL_TIMEOUT: Duration = dispatch::SDK_TIMEOUT;

/// What the web's client answers when the bot cannot send.
pub(crate) const WHATSAPP_NOT_READY: &str =
    "WhatsApp is not paired or not connected. Pair the device from /whatsapp.";

/// The real client for one run: the app and the trader's API key.
pub struct AppFlowClient {
    app: Weak<AppState>,
    api_key: Secret,
}

impl AppFlowClient {
    pub fn new(app: Weak<AppState>, api_key: Secret) -> Self {
        Self { app, api_key }
    }
}

/// What a node reads when its call got no answer: it may have taken
/// effect, so an order is never sent again blindly (parity CL-26, D-04).
pub const NO_ANSWER: &str = "OpenAlgo did not answer in time. If this was an order, it may have been placed: check the order book before running the workflow again.";

/// What a node reads when its call could not be sent at all.
pub const NOT_SENT: &str = "The request could not be sent to OpenAlgo, so nothing was changed.";

/// A call that never got an answer, as a client dict. Anything handed to
/// the handlers may have taken effect, so it is marked unknown.
fn transport_failure(t: Transport) -> Value {
    match t {
        Transport::NotSent => json!({"status": "error", "error": NOT_SENT, "code": 500}),
        Transport::Timeout => {
            json!({"status": "error", "error": NO_ANSWER, "code": 504, "error_type": UNKNOWN_OUTCOME})
        }
        Transport::NoReply | Transport::Budget => {
            json!({"status": "error", "error": NO_ANSWER, "code": 500, "error_type": UNKNOWN_OUTCOME})
        }
    }
}

/// The client dict for what the handlers answered.
fn shaped(raw: dispatch::Raw) -> Value {
    let body: Value = match serde_json::from_str(&raw.text) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            tracing::error!("Flow could not read an API reply (HTTP {})", raw.status);
            return json!({
                "status": "error",
                "error": "OpenAlgo answered in a form Flow could not read.",
                "code": raw.status,
            });
        }
    };
    let mut out = handle_response(raw.status, &body);
    if raw.outcome_unknown {
        if let Some(m) = out.as_object_mut() {
            m.insert("status".into(), json!("error"));
            m.entry("error")
                .or_insert_with(|| body.get("message").cloned().unwrap_or(Value::Null));
            m.insert("code".into(), json!(raw.status));
            m.insert("error_type".into(), json!(UNKNOWN_OUTCOME));
        }
    }
    out
}

/// A broker call answered while no broker is logged in: the web's reply is
/// "Invalid openalgo apikey" (403), which would send a trader to check a
/// key that is fine. A run states the real cause at the node instead
/// (lead decision 16): the desktop's no-broker-session sentence.
fn with_broker_cause(app: &AppState, mut out: Value) -> Value {
    let refused = out.get("code").and_then(Value::as_u64) == Some(403)
        && out.get("error").and_then(Value::as_str) == Some(crate::services::core::INVALID_API_KEY);
    if refused && app.get_broker_session().is_none() {
        out["error"] = json!(crate::strategy::dispatch::NO_BROKER_SESSION);
    }
    out
}

#[async_trait]
impl FlowClient for AppFlowClient {
    async fn api(&self, call: ApiCall) -> Value {
        let Some(app) = self.app.upgrade() else {
            return json!({"status": "error", "error": "OpenAlgo is shutting down.", "code": 503});
        };
        // Strike-mode legs and a scoped close: the public schemas cannot
        // carry them, so they take Flow's internal entry (nodes-orders).
        if let Some(which) = crate::flow::nodes::internal::route(&call) {
            let key = self.api_key.expose();
            return match crate::flow::nodes::internal::run(&app, key, which, call.payload).await {
                Ok(raw) => shaped(raw),
                Err(t) => transport_failure(t),
            };
        }
        let req = match dispatch::post_request(self.api_key.expose(), call.endpoint, call.payload) {
            Ok(r) => r,
            Err(t) => return transport_failure(t),
        };
        match dispatch::run_as(&app, FLOW_CALLER, req, CALL_TIMEOUT).await {
            Ok(raw) => with_broker_cause(&app, shaped(raw)),
            Err(t) => {
                tracing::warn!("Flow {} got no answer: {:?}", call.method, t);
                transport_failure(t)
            }
        }
    }

    async fn telegram(&self, message: &str) -> Value {
        let Some(app) = self.app.upgrade() else {
            return json!({"status": "error", "error": "OpenAlgo is shutting down."});
        };
        // A stopped bot sends nothing, Flow alerts included (web #1577).
        if !crate::messaging::telegram::TelegramService::is_bot_active(&app) {
            tracing::info!("Telegram bot is stopped; skipping Flow alert");
            return json!({"status": "error", "error": "Telegram bot is stopped"});
        }
        let Some(username) = crate::messaging::account_username(&app) else {
            return json!({"status": "error", "error": "Invalid API key"});
        };
        let user = app
            .sqlite
            .conn()
            .and_then(|c| crate::messaging::telegram::db::get_user_by_username(&c, &username));
        let user = match user {
            Ok(Some(u)) => u,
            Ok(None) => {
                tracing::info!("No Telegram account is linked for the Flow alert");
                return json!({"status": "error", "error": format!("No Telegram account linked for user: {}", username)});
            }
            Err(e) => {
                tracing::error!("Could not read the linked Telegram account: {}", e);
                return json!({"status": "error", "error": "The linked Telegram account could not be read."});
            }
        };
        if !user.notifications_enabled {
            return json!({"status": "error", "error": "Telegram notifications are disabled"});
        }
        let text = telegram_text(app.now(), message);
        // Queued, as the web's alert executor queues it: the run goes on.
        let owner = app.clone();
        let telegram_id = user.telegram_id;
        app.spawn(async move {
            owner
                .messaging
                .telegram
                .send_alert(&owner, telegram_id, &text)
                .await;
        });
        json!({"status": "success", "data": {"message": "Alert queued successfully"}})
    }

    async fn whatsapp(&self, message: &str, to: Option<&str>) -> Value {
        let Some(app) = self.app.upgrade() else {
            return json!({"status": "error", "error": "OpenAlgo is shutting down."});
        };
        let wa = &app.messaging.whatsapp;
        let state = (wa.is_ready(&app), wa.unavailable_reason());
        let sender = app.clone();
        whatsapp_reply(state, to, move |targets| async move {
            sender
                .messaging
                .whatsapp
                .send(&sender, &targets, message)
                .await
        })
        .await
    }
}

/// The web client's `whatsapp()` over the service's state `(ready,
/// unavailable reason)` and its send: not ready answers the service's own
/// reason (logged out) or the web's sentence; a `to` that is not 7-15
/// digits is refused; a failed recipient's error (else "send failed") or
/// the report.
pub(crate) async fn whatsapp_reply<F, Fut>(
    (ready, reason): (bool, Option<&str>),
    to: Option<&str>,
    send: F,
) -> Value
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = crate::messaging::whatsapp::Report>,
{
    if !ready {
        return json!({"status": "error", "error": reason.unwrap_or(WHATSAPP_NOT_READY)});
    }
    let targets = match whatsapp_targets(to) {
        Ok(t) => t,
        Err(refused) => return refused,
    };
    whatsapp_answer(&send(targets).await)
}

/// Where a WhatsApp alert goes: the number given, as its chat id, or the
/// paired owner when none is given; a number that is not one is refused.
fn whatsapp_targets(to: Option<&str>) -> Result<Vec<String>, Value> {
    let Some(raw) = to.filter(|t| !t.is_empty()) else {
        return Ok(Vec::new());
    };
    let digits = crate::messaging::whatsapp::normalize_phone(&json!(raw));
    if digits.is_empty() {
        return Err(
            json!({"status": "error", "error": format!("Invalid WhatsApp phone number: {}", raw)}),
        );
    }
    Ok(vec![crate::messaging::whatsapp::phone_to_jid(&digits)])
}

/// The web's answer to a send: its first failure's error (`send failed`
/// when it gave none) with the report, else success with the report.
fn whatsapp_answer(report: &crate::messaging::whatsapp::Report) -> Value {
    let data = serde_json::to_value(report).unwrap_or(Value::Null);
    if let Some(first) = report.failed.first() {
        let error = first.get("error").cloned().unwrap_or(json!("send failed"));
        return json!({"status": "error", "error": error, "data": data});
    }
    json!({"status": "success", "data": data})
}

/// The web's Telegram alert text: a 21-character rule under "Flow Alert",
/// stamped in market time on any machine (lead decision 1; the web wrote
/// the host's local time).
fn telegram_text(now: chrono::DateTime<chrono::Utc>, message: &str) -> String {
    let time = now
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .format("%H:%M:%S");
    let rule = "\u{2500}".repeat(21);
    format!("*Flow Alert*\n{}\n{}\n\nTime: {}", rule, message, time)
}

#[cfg(test)]
mod tests {
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
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let client =
            AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("not-a-key"));
        let v = client.get_quotes("INFY", "NSE").await;
        assert_eq!(v["status"], "error", "{}", v);
        assert!(v["code"].as_u64().is_some_and(|c| c >= 400), "{}", v);
    }

    /// CL-21, CL-22: a stopped Telegram bot or an unpaired WhatsApp device
    /// answers the web's sentences, and nothing is sent.
    #[tokio::test]
    async fn alerts_refuse_as_the_web_refuses() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
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
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
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
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
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
        conn.execute_batch("UPDATE telegram_users SET notifications_enabled = 1")
            .unwrap();
        assert_eq!(
            client.telegram("hi").await,
            json!({"status": "success", "data": {"message": "Alert queued successfully"}})
        );
    }

    /// CL-08: cancel-all sends the `strategy` the desktop's /api/v1
    /// requires, so the API reads the body and answers on the key (403),
    /// not on a missing field (400).
    #[tokio::test]
    async fn cancel_all_sends_the_strategy_the_api_requires() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let client = AppFlowClient::new(std::sync::Arc::downgrade(&t.ctx), Secret::new("k"));
        let v = client.cancel_all_orders().await;
        assert_eq!(v["status"], "error", "{}", v);
        assert_eq!(v["code"], 403, "{}", v);
    }
}
