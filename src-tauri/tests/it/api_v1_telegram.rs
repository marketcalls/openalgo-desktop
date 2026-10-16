//! API-01: the `/api/v1/telegram` bot management endpoints, written from the
//! web's source (`restx_api/telegram_bot.py`, `database/telegram_db.py`).
//! Nothing here reaches Telegram: the bot is never started with a token.

use crate::api_v1_support::H;
use axum::body::Body;
use axum::http::{header, Method, Request};
use openalgo_desktop_lib::messaging::telegram::db::{self, Link};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TOKEN: &str = "1234567890:AAEXAMPLEEXAMPLEEXAMPLEEXAMPLEexamp";

async fn get(h: &H, path: &str, key_header: Option<&str>) -> (u16, Value) {
    let mut b = Request::builder().method(Method::GET).uri(path);
    if let Some(k) = key_header {
        b = b.header("x-api-key", k);
    }
    let (s, _, bytes) = h.send(b.body(Body::empty()).unwrap()).await;
    (
        s.as_u16(),
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post(h: &H, path: &str, body: Value) -> (u16, Value) {
    let (s, v) = h.post(path, body).await;
    (s.as_u16(), v)
}

fn link(h: &H, telegram_id: i64, broker: &str) {
    let conn = h.ctx.sqlite.conn().unwrap();
    let l = Link {
        telegram_id,
        username: "trader",
        api_key: None,
        host_url: None,
        first_name: "T",
        last_name: "",
        telegram_username: "t_user",
        broker,
    };
    db::create_or_update_user(&conn, &h.ctx.security, &l, h.ctx.now()).unwrap();
}

fn bad_key() -> (u16, Value) {
    (
        401,
        json!({"status": "error", "message": "Invalid or missing API key"}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_key_comes_from_the_web_places_and_a_bad_one_is_401() {
    let h = H::new().await;
    assert_eq!(get(&h, "/api/v1/telegram/config", None).await, bad_key());
    assert_eq!(
        get(&h, "/api/v1/telegram/config?apikey=wrong", None).await,
        bad_key()
    );
    let q = format!("/api/v1/telegram/config?apikey={}", h.key);
    assert_eq!(get(&h, &q, None).await.0, 200);
    assert_eq!(
        get(&h, "/api/v1/telegram/stats", Some(h.key.as_str()))
            .await
            .0,
        200
    );
    assert_eq!(
        post(&h, "/api/v1/telegram/config", json!({"apikey": "wrong"})).await,
        bad_key()
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_are_saved_and_the_token_is_never_shown_whole() {
    let h = H::new().await;
    let (s, v) = post(
        &h,
        "/api/v1/telegram/config",
        h.with_key(json!({"token": TOKEN, "broadcast_enabled": false,
            "rate_limit_per_minute": 60, "webhook_url": "ignored", "polling_mode": true})),
    )
    .await;
    assert_eq!(
        (s, v),
        (
            200,
            json!({"status": "success", "message": "Bot configuration updated"})
        )
    );
    let (s, v) = get(&h, "/api/v1/telegram/config", Some(h.key.as_str())).await;
    assert_eq!(s, 200, "{}", v);
    assert!(
        !v.to_string().contains(TOKEN),
        "the whole token leaked: {}",
        v
    );
    let data = &v["data"];
    assert_eq!(data["bot_token"], "1234567890...");
    assert_eq!(data["token"], "1234567890...");
    assert_eq!(data["broadcast_enabled"], false);
    assert_eq!(data["rate_limit_per_minute"], 60);
    assert_eq!(data["max_message_length"], 4096);
    assert_eq!(data["is_active"], false);
    for key in ["bot_username", "created_at", "updated_at"] {
        assert!(data.get(key).is_some(), "{} missing: {}", key, v);
    }
    // The shortened token the settings show, or a token that is not text,
    // never replaces the real one.
    for (token, msg) in [
        (
            json!("1234567890..."),
            "Send the whole bot token from BotFather, not the shortened one the settings show.",
        ),
        (json!(123), "The bot token must be text."),
    ] {
        let body = h.with_key(json!({ "token": token }));
        assert_eq!(
            post(&h, "/api/v1/telegram/config", body).await,
            (400, json!({"status": "error", "message": msg}))
        );
    }
    let (_, v) = get(&h, "/api/v1/telegram/config", Some(h.key.as_str())).await;
    assert_eq!(v["data"]["token"], "1234567890...");
    for (rate, msg) in [
        (json!(0), "rate_limit_per_minute must be between 1 and 120"),
        (
            json!(121),
            "rate_limit_per_minute must be between 1 and 120",
        ),
        (json!("abc"), "rate_limit_per_minute must be an integer"),
    ] {
        let body = h.with_key(json!({"rate_limit_per_minute": rate}));
        assert_eq!(
            post(&h, "/api/v1/telegram/config", body).await,
            (400, json!({"status": "error", "message": msg}))
        );
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_needs_a_token_and_stop_needs_a_running_bot() {
    let h = H::new().await;
    assert_eq!(
        post(&h, "/api/v1/telegram/start", h.with_key(json!({}))).await,
        (
            400,
            json!({"status": "error", "message": "Bot token not configured"})
        )
    );
    assert_eq!(
        post(&h, "/api/v1/telegram/stop", h.with_key(json!({}))).await,
        (
            500,
            json!({"status": "error", "message": "Bot is not running"})
        )
    );
    // No body at all, the key in the header.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/telegram/stop")
        .header("x-api-key", h.key.as_str())
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.send(req).await.0.as_u16(), 500);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn users_are_listed_with_the_web_filters() {
    let h = H::new().await;
    link(&h, 111, "zerodha");
    link(&h, 222, "angel");
    let (s, v) = get(&h, "/api/v1/telegram/users", Some(h.key.as_str())).await;
    assert_eq!(s, 200, "{}", v);
    assert_eq!(v["count"], 2);
    let first = &v["data"][0];
    for key in [
        "id",
        "telegram_id",
        "openalgo_username",
        "first_name",
        "last_name",
        "telegram_username",
        "broker",
        "notifications_enabled",
        "created_at",
        "last_command_at",
    ] {
        assert!(first.get(key).is_some(), "{} missing: {}", key, first);
    }
    let (_, v) = get(
        &h,
        "/api/v1/telegram/users?broker=angel",
        Some(h.key.as_str()),
    )
    .await;
    assert_eq!(v["count"], 1);
    assert_eq!(v["data"][0]["telegram_id"], 222);
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_checks_the_message_and_the_switch() {
    let h = H::new().await;
    let path = "/api/v1/telegram/broadcast";
    let error = |m: &str| json!({"status": "error", "message": m});
    assert_eq!(
        post(&h, path, h.with_key(json!({}))).await,
        (400, error("Message is required"))
    );
    assert_eq!(
        post(&h, path, h.with_key(json!({"message": "x".repeat(4097)}))).await,
        (400, error("Message must not exceed 4096 characters"))
    );
    assert_eq!(
        post(&h, path, h.with_key(json!({"message": "hi"}))).await,
        (
            200,
            json!({"status": "success", "message": "Broadcast sent to 0 users, failed for 0 users",
                "success_count": 0, "fail_count": 0})
        )
    );
    let off = h.with_key(json!({"broadcast_enabled": false}));
    assert_eq!(post(&h, "/api/v1/telegram/config", off).await.0, 200);
    assert_eq!(
        post(&h, path, h.with_key(json!({"message": "hi"}))).await,
        (403, error("Broadcast is disabled"))
    );
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_take_one_to_365_days() {
    let h = H::new().await;
    for (q, days) in [
        ("", 7),
        ("?days=30", 30),
        ("?days=abc", 7),
        ("?days=1000", 365),
    ] {
        let (s, v) = get(
            &h,
            &format!("/api/v1/telegram/stats{}", q),
            Some(h.key.as_str()),
        )
        .await;
        assert_eq!(s, 200, "{}", v);
        assert_eq!(
            v["data"],
            json!({"total_commands": 0, "commands_by_type": {}, "active_users": 0,
                "top_users": [], "period_days": days}),
            "{}",
            q
        );
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preferences_default_then_follow_what_was_saved() {
    let h = H::new().await;
    let path = "/api/v1/telegram/preferences";
    let required = (
        400,
        json!({"status": "error", "message": "telegram_id is required"}),
    );
    assert_eq!(get(&h, path, Some(h.key.as_str())).await, required);
    assert_eq!(
        get(
            &h,
            &format!("{}?telegram_id=abc", path),
            Some(h.key.as_str())
        )
        .await,
        required
    );
    assert_eq!(post(&h, path, h.with_key(json!({}))).await, required);
    let read = format!("{}?telegram_id=111", path);
    assert_eq!(
        get(&h, &read, Some(h.key.as_str())).await.1["data"],
        json!({"order_notifications": true, "trade_notifications": true,
            "pnl_notifications": true, "daily_summary": true, "summary_time": "18:00",
            "language": "en", "timezone": "Asia/Kolkata"})
    );
    let save = h.with_key(json!({"telegram_id": 111, "daily_summary": false,
        "summary_time": "16:30", "unknown": 1}));
    assert_eq!(
        post(&h, path, save).await,
        (
            200,
            json!({"status": "success", "message": "Preferences updated successfully"})
        )
    );
    let data = get(&h, &read, Some(h.key.as_str())).await.1["data"].clone();
    assert_eq!(data["daily_summary"], false);
    assert_eq!(data["summary_time"], "16:30");
    assert_eq!(data["order_notifications"], true);
    let wrong = h.with_key(json!({"telegram_id": 111, "daily_summary": "no"}));
    assert_eq!(
        post(&h, path, wrong).await,
        (
            400,
            json!({"status": "error", "message": "daily_summary must be true or false"})
        )
    );
    h.shutdown().await;
}

async fn webhook(h: &H, secret: Option<&str>, body: &str) -> (u16, String) {
    let mut b = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/telegram/webhook")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(s) = secret {
        b = b.header("x-telegram-bot-api-secret-token", s);
    }
    let (s, _, bytes) = h.send(b.body(Body::from(body.to_string())).unwrap()).await;
    (s.as_u16(), String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_webhook_answers_only_telegram() {
    let h = H::new().await;
    assert_eq!(
        webhook(&h, None, r#"{"update_id": 1}"#).await,
        (200, String::new())
    );
    let body = h.with_key(json!({"token": TOKEN}));
    assert_eq!(post(&h, "/api/v1/telegram/config", body).await.0, 200);
    let secret = hex::encode(Sha256::digest(TOKEN.as_bytes()))[..32].to_string();
    assert_eq!(
        webhook(&h, None, r#"{"update_id": 1}"#).await,
        (401, "Unauthorized".to_string())
    );
    assert_eq!(
        webhook(&h, Some("wrong"), r#"{"update_id": 1}"#).await,
        (403, "Forbidden".to_string())
    );
    assert_eq!(
        webhook(&h, Some(secret.as_str()), r#"{"update_id": 1}"#).await,
        (200, String::new())
    );
    assert_eq!(
        webhook(&h, Some(secret.as_str()), r#"{"message": {}}"#).await,
        (400, "Bad Request".to_string())
    );
    h.shutdown().await;
}

/// The web's limits on the namespace: 30 per minute per caller, and 5 per
/// minute for a broadcast, so a script cannot flood the linked chats.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_namespace_keeps_the_web_limits() {
    let h = H::new().await;
    let caller: std::net::IpAddr = "10.9.9.9".parse().unwrap();
    let stats = || {
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/telegram/stats")
            .header("x-api-key", h.key.as_str())
            .body(Body::empty())
            .unwrap()
    };
    for _ in 0..30 {
        assert_eq!(h.send_from(stats(), caller).await.0.as_u16(), 200);
    }
    let (s, _, b) = h.send_from(stats(), caller).await;
    assert_eq!(s.as_u16(), 429);
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap(),
        json!({"message": "30 per 1 minute"})
    );

    let other: std::net::IpAddr = "10.9.9.10".parse().unwrap();
    let broadcast = || {
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/telegram/broadcast")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(h.with_key(json!({"message": "hi"})).to_string()))
            .unwrap()
    };
    for _ in 0..5 {
        assert_eq!(h.send_from(broadcast(), other).await.0.as_u16(), 200);
    }
    let (s, _, b) = h.send_from(broadcast(), other).await;
    assert_eq!(s.as_u16(), 429);
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap(),
        json!({"message": "5 per 1 minute"})
    );
    h.shutdown().await;
}
