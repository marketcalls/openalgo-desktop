//! The real client's alert paths (core's `AppFlowClient`, which the alert
//! nodes call): Telegram through the native service with the web's four
//! refusals and a send queued without waiting (parity N-telegramAlert-03,
//! -06, D-27, X-153), WhatsApp with the web's refusals, failure and report
//! shapes (N-whatsappAlert-03, -04, D-28, X-342).

use crate::brokers::BrokerRegistry;
use crate::flow::contract::client::app::{whatsapp_reply, AppFlowClient, WHATSAPP_NOT_READY};
use crate::flow::contract::client::FlowClient;
use crate::messaging::telegram::db as tg_db;
use crate::messaging::whatsapp::{Report, LOGGED_OUT_MESSAGE};
use crate::security::Secret;
use crate::state::testing::{build, TestCtx};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TELEGRAM_ID: i64 = 101;

fn app() -> TestCtx {
    build(BrokerRegistry::new(), chrono::Utc::now())
}

fn client(t: &TestCtx) -> AppFlowClient {
    AppFlowClient::new(Arc::downgrade(&t.ctx), Secret::new("k"))
}

/// The bot switched on, with a token when `token` is given.
fn activate(t: &TestCtx, token: Option<String>) {
    let c = t.ctx.sqlite.conn().unwrap();
    tg_db::update_bot_config(
        &c,
        &t.ctx.security,
        tg_db::ConfigUpdate {
            token: token.map(Some),
            is_active: Some(true),
            ..Default::default()
        },
        t.ctx.now(),
    )
    .unwrap();
}

fn sign_up(t: &TestCtx) {
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
}

fn link(t: &TestCtx) {
    let c = t.ctx.sqlite.conn().unwrap();
    tg_db::create_or_update_user(
        &c,
        &t.ctx.security,
        &tg_db::Link {
            telegram_id: TELEGRAM_ID,
            username: "trader",
            api_key: None,
            host_url: None,
            first_name: "Asha",
            last_name: "",
            telegram_username: "asha",
            broker: "zerodha",
        },
        t.ctx.now(),
    )
    .unwrap();
}

#[tokio::test]
async fn telegram_refuses_as_the_web_refuses() {
    let t = app();
    activate(&t, None);
    assert_eq!(
        client(&t).telegram("hi").await,
        json!({"status": "error", "error": "Invalid API key"})
    );
    // Creating the account makes new keys and drops what was saved before
    // them, the bot's switch included (key_vault::drop_encrypted_values).
    sign_up(&t);
    activate(&t, None);
    assert_eq!(
        client(&t).telegram("hi").await,
        json!({"status": "error", "error": "No Telegram account linked for user: trader"})
    );
    link(&t);
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE telegram_users SET notifications_enabled = 0 WHERE telegram_id = ?1",
            [TELEGRAM_ID],
        )
        .unwrap();
    assert_eq!(
        client(&t).telegram("hi").await,
        json!({"status": "error", "error": "Telegram notifications are disabled"})
    );
}

/// A Bot API stand-in that records each request and answers only when
/// released.
struct HeldBotApi {
    seen: Mutex<Vec<(String, Value)>>,
    release: tokio::sync::Notify,
    arrived: tokio::sync::Notify,
}

async fn held_bot_api() -> (Arc<HeldBotApi>, String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let api = Arc::new(HeldBotApi {
        seen: Mutex::default(),
        release: tokio::sync::Notify::new(),
        arrived: tokio::sync::Notify::new(),
    });
    let held = api.clone();
    let task = tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let (head_end, length) = loop {
            let Ok(n) = sock.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                break (i + 4, length);
            }
        };
        while buf.len() < head_end + length {
            let Ok(n) = sock.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
        let body = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
        held.seen.lock().push((path, body));
        held.arrived.notify_one();
        held.release.notified().await;
        let reply = r#"{"ok":true,"result":{"message_id":1}}"#;
        let _ = sock
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    reply.len(),
                    reply
                )
                .as_bytes(),
            )
            .await;
    });
    (api, base, task)
}

/// X-153, N-telegramAlert-06: the alert goes to the app account's linked
/// Telegram id with the web's text, and the run does not wait for it (the
/// Bot API here answers only after the client has returned).
#[tokio::test]
async fn telegram_queues_the_alert_to_the_owner_without_waiting() {
    let t = app();
    let token = format!("{}:{}", 123456, "FLOW-TEST");
    sign_up(&t);
    activate(&t, Some(token.clone()));
    link(&t);
    let (api, base, task) = held_bot_api().await;
    t.ctx.messaging.telegram.set_api_base(&base);
    let answer = tokio::time::timeout(
        Duration::from_secs(5),
        client(&t).telegram("Owner-only notice"),
    )
    .await
    .expect("the client waited for the send");
    assert_eq!(
        answer,
        json!({"status": "success", "data": {"message": "Alert queued successfully"}})
    );
    tokio::time::timeout(Duration::from_secs(10), api.arrived.notified())
        .await
        .expect("the alert was not sent");
    let (path, body) = api.seen.lock()[0].clone();
    assert_eq!(path, format!("/bot{}/sendMessage", token));
    assert_eq!(body["chat_id"], json!(TELEGRAM_ID));
    let text = body["text"].as_str().unwrap_or("");
    assert!(text.starts_with("*Flow Alert*\n"), "{}", text);
    assert!(text.contains("\nOwner-only notice\n"), "{}", text);
    api.release.notify_one();
    task.await.unwrap();
}

fn report(sent: &[&str], failed: Vec<Value>) -> Report {
    Report {
        sent: sent.iter().map(|s| s.to_string()).collect(),
        failed,
        skipped: 0,
    }
}

fn no_send() -> Report {
    panic!("nothing may be sent")
}

/// N-whatsappAlert-03/-04, D-28: the client's answers over the service's
/// state and report.
#[tokio::test]
async fn whatsapp_answers_as_the_web_client() {
    let never = |_t: Vec<String>| async { no_send() };
    assert_eq!(
        whatsapp_reply((false, None), None, never).await,
        json!({"status": "error", "error": WHATSAPP_NOT_READY})
    );
    assert_eq!(
        whatsapp_reply((false, Some(LOGGED_OUT_MESSAGE)), None, never).await,
        json!({"status": "error", "error": LOGGED_OUT_MESSAGE})
    );
    assert_eq!(
        whatsapp_reply((true, None), Some("call me"), never).await,
        json!({"status": "error", "error": "Invalid WhatsApp phone number: call me"})
    );
    let to = Arc::new(Mutex::new(Vec::new()));
    let seen = to.clone();
    let ok = whatsapp_reply((true, None), Some("+91 98765 43210"), move |t| async move {
        seen.lock().extend(t);
        report(&["919876543210@s.whatsapp.net"], vec![])
    })
    .await;
    assert_eq!(*to.lock(), vec!["919876543210@s.whatsapp.net".to_string()]);
    assert_eq!(
        ok,
        json!({"status": "success", "data": {"sent": ["919876543210@s.whatsapp.net"], "failed": [], "skipped": 0}})
    );
    // No number: the paired device itself (no target handed to the send).
    let own = whatsapp_reply((true, None), None, |t| async move {
        assert!(t.is_empty());
        report(&["self"], vec![])
    })
    .await;
    assert_eq!(own["status"], "success");
    let failed = whatsapp_reply((true, None), Some("919876543210"), |_| async {
        report(&[], vec![json!({"to": "x", "error": "send timeout"})])
    })
    .await;
    assert_eq!(
        failed,
        json!({"status": "error", "error": "send timeout",
               "data": {"sent": [], "failed": [{"to": "x", "error": "send timeout"}], "skipped": 0}})
    );
    let bare = whatsapp_reply((true, None), Some("919876543210"), |_| async {
        report(&[], vec![json!({"to": "x"})])
    })
    .await;
    assert_eq!(bare["error"], "send failed");
}

/// X-342: once WhatsApp has logged the device out, the Flow client (and so
/// the node) answers the service's own logged-out sentence.
#[tokio::test]
async fn a_logged_out_whatsapp_answers_its_sentence() {
    let t = app();
    t.ctx.messaging.whatsapp.mark_logged_out();
    assert_eq!(
        client(&t).whatsapp("09:15 alert", None).await,
        json!({"status": "error", "error": LOGGED_OUT_MESSAGE})
    );
}
