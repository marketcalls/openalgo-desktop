//! The /alerts page API and the Slack and Discord notify endpoints, through
//! the full router (web `test/test_alerts_api.py`). The page API changes
//! settings and saves webhooks, so it needs the signed-in session and the
//! CSRF token, and it never returns a webhook URL; the notify endpoints
//! need an API key and can only post.

use super::*;
use crate::messaging::channels::testkit::{Provider, DISCORD_URL, SECRETS, SLACK_URL};
use crate::messaging::channels::{settings, ChannelName};

struct A {
    h: H,
    key: String,
    cookie: String,
    csrf: String,
    provider: Arc<Provider>,
}

impl A {
    fn new() -> Self {
        let h = H::new();
        let key = h.setup();
        let (cookie, csrf) = h.session(true);
        let provider = Arc::new(Provider::default());
        h.ctx().messaging.channels.http.set(provider.clone());
        A {
            h,
            key,
            cookie,
            csrf,
            provider,
        }
    }

    async fn page(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if body.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        let req = b
            .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
            .unwrap();
        self.h
            .json(with_session(req, &self.cookie, Some(&self.csrf)))
            .await
    }

    async fn notify(&self, channel: &str, extra: Value) -> (StatusCode, Value) {
        let mut body = json!({"apikey": self.key, "message": "hi"});
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        self.h
            .json(post_json(&format!("/api/v1/{}/notify", channel), body))
            .await
    }

    async fn save(&self, channel: &str, url: &str) -> (StatusCode, Value) {
        self.page(
            Method::PUT,
            &format!("/alerts/channels/{}/webhook", channel),
            Some(json!({"url": url})),
        )
        .await
    }
}

fn channel<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["data"]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap()
}

/// Web test/test_alerts_api.py::test_the_page_lists_every_channel_and_group
#[tokio::test]
async fn the_page_lists_every_channel_and_group() {
    let a = A::new();
    let (s, body) = a.page(Method::GET, "/alerts/channels", None).await;
    assert_eq!(s, StatusCode::OK, "{}", body);
    let names: Vec<&str> = body["data"]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["telegram", "whatsapp", "slack", "discord"]);
    assert_eq!(body["data"]["categories"][0]["id"], "orders_placed");
    assert_eq!(channel(&body, "telegram")["enabled"], true);
    assert_eq!(channel(&body, "slack")["enabled"], false);
    assert_eq!(channel(&body, "slack")["setup_path"], "/slack");
    assert_eq!(channel(&body, "slack")["uses_webhook"], true);
    assert_eq!(
        channel(&body, "telegram")["categories"],
        json!([
            "batch_orders",
            "gtt_orders",
            "orders_cancelled",
            "orders_modified",
            "orders_placed",
            "positions_closed"
        ]),
        "sorted, as the web lists them"
    );
    assert_eq!(
        channel(&body, "discord")["status"],
        json!({"configured": false, "ready": false,
            "detail": "No webhook is saved yet. Add one on the Discord page."})
    );
}

/// Web test/test_alerts_api.py::test_the_page_api_needs_a_signed_in_session
/// and ::test_a_change_needs_the_csrf_token
#[tokio::test]
async fn the_page_api_needs_the_signed_in_session_and_the_csrf_token() {
    let a = A::new();
    let anonymous = Request::builder()
        .uri("/alerts/channels")
        .header(header::ACCEPT, "application/json")
        .body(Body::empty())
        .unwrap();
    let (s, _) = a.h.json(anonymous).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let no_token = Request::builder()
        .method(Method::PUT)
        .uri("/alerts/channels/slack/webhook")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"url": SLACK_URL}).to_string()))
        .unwrap();
    let (s, _) = a.h.json(with_session(no_token, &a.cookie, None)).await;
    assert_ne!(s, StatusCode::OK);
    assert!(crate::messaging::channels::webhook::saved_url(
        a.h.ctx(),
        crate::messaging::channels::webhook::WebhookKind::Slack
    )
    .unwrap()
    .is_none());
}

/// Web test/test_alerts_api.py::test_a_channel_can_be_switched_off_and_rerouted
#[tokio::test]
async fn a_channel_can_be_switched_off_and_rerouted() {
    let a = A::new();
    let (s, body) = a
        .page(
            Method::POST,
            "/alerts/channels/telegram",
            Some(json!({"enabled": false, "categories": ["gtt_orders"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", body);
    assert_eq!(body["data"]["enabled"], false);
    let now = settings::settings_for(a.h.ctx(), ChannelName::Telegram);
    assert_eq!(
        now.categories.into_iter().collect::<Vec<_>>(),
        ["gtt_orders"]
    );
}

/// Web test/test_alerts_api.py::test_a_bad_change_is_refused,
/// ::test_an_unknown_channel_is_404 and
/// ::test_slack_cannot_be_switched_on_without_a_webhook
#[tokio::test]
async fn a_bad_change_is_refused() {
    let a = A::new();
    for (body, code, says) in [
        (json!({"enabled": "yes"}), 400, "true or false"),
        (json!({"categories": "orders_placed"}), 400, "list"),
        (
            json!({"categories": ["orders_placed", "bogus"]}),
            400,
            "Unknown event group",
        ),
        (json!([1]), 400, "Send the settings as a JSON object."),
    ] {
        let (s, v) = a
            .page(Method::POST, "/alerts/channels/telegram", Some(body))
            .await;
        assert_eq!(s.as_u16(), code, "{}", v);
        assert!(v["message"].as_str().unwrap().contains(says), "{}", v);
    }
    let (s, _) = a
        .page(
            Method::POST,
            "/alerts/channels/sms",
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = a.save("telegram", SLACK_URL).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = a
        .page(
            Method::POST,
            "/alerts/channels/slack",
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v["message"],
        "Save a Slack webhook before turning Slack on."
    );
}

/// Web test/test_alerts_api.py::test_a_saved_webhook_is_never_sent_back,
/// ::test_a_webhook_for_the_wrong_provider_is_refused and
/// ::test_forgetting_a_webhook_turns_the_channel_off
#[tokio::test]
async fn a_saved_webhook_is_never_sent_back() {
    let a = A::new();
    let (s, saved) = a.save("slack", SLACK_URL).await;
    assert_eq!(s, StatusCode::OK, "{}", saved);
    assert_eq!(saved["data"]["enabled"], true);
    assert_eq!(saved["data"]["status"]["configured"], true);
    let (_, listed) = a.page(Method::GET, "/alerts/channels", None).await;
    for text in [saved.to_string(), listed.to_string()] {
        assert!(!text.contains(SECRETS[0]) && !text.contains("hooks.slack.com"));
    }
    let (s, v) = a.save("slack", DISCORD_URL).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("not a Slack webhook URL"));
    a.save("discord", DISCORD_URL).await;
    let (s, v) = a
        .page(Method::DELETE, "/alerts/channels/discord/webhook", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["enabled"], false);
}

/// Web test/test_alerts_api.py::test_a_test_message_reports_what_happened,
/// and the web's five tests a minute.
#[tokio::test]
async fn a_test_message_reports_what_happened() {
    let a = A::new();
    a.save("slack", SLACK_URL).await;
    let (s, v) = a
        .page(Method::POST, "/alerts/channels/slack/test", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["message"], "Message sent to Slack.");
    *a.provider.status.lock() = 404;
    let (s, v) = a
        .page(Method::POST, "/alerts/channels/slack/test", None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("Create a new webhook"));
    let (s, v) = a
        .page(Method::POST, "/alerts/channels/telegram/test", None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v["message"],
        "No bot token is saved. Add one on the Telegram page."
    );
    for _ in 0..2 {
        a.page(Method::POST, "/alerts/channels/discord/test", None)
            .await;
    }
    let (s, v) = a
        .page(Method::POST, "/alerts/channels/discord/test", None)
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "the sixth in a minute");
    assert!(v["message"].as_str().unwrap().contains("Wait a minute"));
}

/// Web test/test_alerts_api.py::test_notify_needs_a_valid_api_key,
/// ::test_notify_does_not_take_the_key_from_the_query_string and
/// ::test_notify_takes_the_key_from_the_header
#[tokio::test]
async fn notify_needs_a_valid_key_from_the_body_or_the_header() {
    let a = A::new();
    a.save("slack", SLACK_URL).await;
    let (s, v) = a.notify("slack", json!({"apikey": "bad"})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        v,
        json!({"status": "error", "message": "Invalid or missing API key"})
    );
    let (s, _) =
        a.h.json(post_json("/api/v1/slack/notify", json!({"message": "hi"})))
            .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) =
        a.h.json(post_json(
            &format!("/api/v1/slack/notify?apikey={}", a.key),
            json!({"message": "hi"}),
        ))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "never from the query string");
    let mut by_header = post_json("/api/v1/slack/notify", json!({"message": "hi"}));
    by_header
        .headers_mut()
        .insert("x-api-key", a.key.parse().unwrap());
    let (s, v) = a.h.json(by_header).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Notification queued."})
    );
}

/// Web test/test_alerts_api.py::test_notify_checks_its_input, and a body
/// that is not a JSON object.
#[tokio::test]
async fn notify_checks_its_input() {
    let a = A::new();
    a.save("discord", DISCORD_URL).await;
    for (extra, says) in [
        (json!({"message": ""}), "'message' is required."),
        (json!({"message": 7}), "'message' is required."),
        (
            json!({"message": "x".repeat(2001)}),
            "'message' must be at most 2000 characters.",
        ),
        (
            json!({"wait_for_delivery": "yes"}),
            "'wait_for_delivery' must be true or false.",
        ),
    ] {
        let (s, v) = a.notify("discord", extra).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
        assert_eq!(v["message"], says);
    }
    let not_json = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/discord/notify")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("hi"))
        .unwrap();
    let (s, v) = a.h.json(not_json).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Send the request as a JSON object.");
}

/// Web test/test_alerts_api.py::test_notify_needs_the_channel_switched_on
/// and ::test_notify_waits_for_delivery_when_asked
#[tokio::test]
async fn notify_needs_the_channel_on_and_can_wait_for_delivery() {
    let a = A::new();
    let (s, v) = a.notify("discord", json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v["message"],
        "Discord alerts are turned off. Turn them on from the Alerts page in OpenAlgo."
    );
    a.save("discord", DISCORD_URL).await;
    *a.provider.status.lock() = 204;
    let (s, v) = a
        .notify(
            "discord",
            json!({"message": "@everyone buy", "wait_for_delivery": true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["message"], "Message sent to Discord.");
    assert_eq!(
        a.provider.body(0),
        json!({"content": "@everyone buy", "allowed_mentions": {"parse": []}})
    );
}
