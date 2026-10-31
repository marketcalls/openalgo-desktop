//! The webhook settings and the public webhook over HTTP, against the
//! web's `blueprints/flow.py`: every key each settings answer carries, the
//! auth type's exact spellings, activation and the webhook switch apart,
//! the path symbol, a locked app, tunnel callers and the body cap's edge
//! (parity R-29, R-32, R-40, R-41, R-43..R-46, R-49, S-07, T-109, D-06).

use super::*;

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

fn is_hex_secret(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() == 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
}

const SETTINGS_KEYS: [&str; 7] = [
    "webhook_auth_type",
    "webhook_enabled",
    "webhook_secret",
    "webhook_token",
    "webhook_url",
    "webhook_url_with_secret",
    "webhook_url_with_symbol",
];

/// R-40, R-41, R-43..R-46, S-07: GET, enable, auth-type, regenerate and
/// regenerate-secret answer exactly the web's keys and values; the auth
/// type is case-sensitive; the secret is 64 lower-case hex characters.
#[tokio::test]
async fn every_settings_answer_carries_the_webs_keys() {
    let f = F::new();
    let id = f.create(webhook_graph("Keys")).await;
    let path = |p: &str| format!("/flow/api/workflows/{}/webhook{}", id, p);
    let base = format!("http://127.0.0.1:{}", f.h.ctx().listening_port());
    let (s, info) = f.call(Method::GET, &path(""), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(keys(&info), SETTINGS_KEYS);
    assert_eq!(info["webhook_enabled"], false);
    assert!(is_hex_secret(&info["webhook_secret"]), "{}", info);
    let token = info["webhook_token"].as_str().unwrap().to_string();
    let secret = info["webhook_secret"].as_str().unwrap().to_string();
    let url = format!("{}/flow/webhook/{}", base, token);

    let (s, on) = f.post(&path("/enable")).await;
    assert_eq!(s, StatusCode::OK);
    let mut want: Vec<&str> = SETTINGS_KEYS.to_vec();
    want.extend(["message", "status"]);
    want.sort_unstable();
    assert_eq!(keys(&on), want);
    assert_eq!(
        (
            on["webhook_auth_type"].clone(),
            on["webhook_url_with_secret"].clone()
        ),
        (json!("payload"), Value::Null)
    );
    let (_, info) = f.call(Method::GET, &path(""), None).await;
    assert_eq!(info["webhook_enabled"], true, "GET reads the switch");

    let auth = |t: &str| json!({"auth_type": t}).to_string();
    for wrong in ["URL", "Payload", ""] {
        let (s, v) = f
            .call(Method::POST, &path("/auth-type"), Some(&auth(wrong)))
            .await;
        assert_eq!(
            (s, v),
            (
                StatusCode::BAD_REQUEST,
                json!({"error": "Invalid auth type"})
            ),
            "{:?}",
            wrong
        );
    }
    let (s, v) = f
        .call(Method::POST, &path("/auth-type"), Some(&auth("url")))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Webhook auth type set to 'url'",
                "webhook_auth_type": "url", "webhook_url": url,
                "webhook_url_with_secret": format!("{}?secret={}", url, secret)})
        )
    );
    let (_, on) = f.post(&path("/enable")).await;
    assert_eq!(
        on["webhook_url_with_secret"],
        format!("{}?secret={}", url, secret)
    );
    let (s, v) = f
        .call(Method::POST, &path("/auth-type"), Some(&auth("payload")))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Webhook auth type set to 'payload'",
                "webhook_auth_type": "payload", "webhook_url": url,
                "webhook_url_with_secret": null})
        )
    );

    let (s, v) = f.post(&path("/regenerate-secret")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(is_hex_secret(&v["webhook_secret"]), "{}", v);
    assert_ne!(v["webhook_secret"], json!(secret));
    assert_eq!(
        v,
        json!({"status": "success", "message": "Webhook secret regenerated",
            "webhook_secret": v["webhook_secret"]})
    );

    let (s, v) = f.post(&path("/regenerate")).await;
    assert_eq!(s, StatusCode::OK);
    let fresh = v["webhook_token"].as_str().unwrap().to_string();
    assert_ne!(fresh, token);
    assert!(is_hex_secret(&v["webhook_secret"]), "{}", v);
    let fresh_url = format!("{}/flow/webhook/{}", base, fresh);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Webhook token and secret regenerated",
            "webhook_token": fresh, "webhook_secret": v["webhook_secret"],
            "webhook_url": fresh_url,
            "webhook_url_with_symbol": format!("{}/{{symbol}}", fresh_url)})
    );
}

/// R-29, R-32: activation does not turn the webhook on and deactivation
/// does not turn it off: an activated workflow's webhook answers 403
/// disabled until enabled, and a deactivated one keeps its switch on and
/// answers 403 not active.
#[tokio::test]
async fn activation_and_the_webhook_switch_are_apart() {
    let f = F::new();
    let id = f.create(webhook_graph("Apart")).await;
    let settings = format!("/flow/api/workflows/{}/webhook", id);
    let (_, info) = f.call(Method::GET, &settings, None).await;
    let url = format!("/flow/webhook/{}", info["webhook_token"].as_str().unwrap());
    let body = json!({"secret": info["webhook_secret"]}).to_string();
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (_, info) = f.call(Method::GET, &settings, None).await;
    assert_eq!(info["webhook_enabled"], false);
    let (s, _, v) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "Webhook is disabled"})
        )
    );
    f.post(&format!("{}/enable", settings)).await;
    let (s, _, _) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = f
        .post(&format!("/flow/api/workflows/{}/deactivate", id))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, info) = f.call(Method::GET, &settings, None).await;
    assert_eq!(info["webhook_enabled"], true, "the switch is kept");
    let (s, _, v) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "Workflow is not active"})
        )
    );
}

/// R-49: the path's symbol is URL-decoded and keeps its case, as Flask's
/// path converter hands it over.
#[tokio::test]
async fn the_path_symbol_is_decoded_and_keeps_its_case() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("Symbols").await;
    let body = json!({"secret": secret, "symbol": "IGNORED", "qty": 5}).to_string();
    for (segment, symbol) in [
        ("M%26M", "M&M"),
        ("sbin", "sbin"),
        ("Nifty%2050", "Nifty 50"),
    ] {
        let (s, _, v) = f
            .send_hook(
                &format!("/flow/webhook/{}/{}", token, segment),
                &body,
                "127.0.0.1",
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}: {}", segment, v);
        let logs = f.last_run_logs(id).await;
        assert!(
            logs.contains(&format!(
                "[LOG] Got {} x5 secret={{{{webhook.secret}}}}",
                symbol
            )),
            "{}: {:?}",
            segment,
            logs
        );
    }
}

/// D-06 (decision 16): while OpenAlgo is locked a known token is refused
/// with the locked sentence, word for word; an unknown token is still the
/// web's 404 (nothing about the lock is told to a guesser).
#[tokio::test]
async fn a_locked_app_refuses_a_known_token_and_does_not_know_an_unknown_one() {
    let f = F::new();
    let (_, token, secret) = f.live_webhook("Locked out").await;
    f.h.ctx().security.lock_with(|| {});
    let body = json!({"secret": secret}).to_string();
    let (s, _, v) = f
        .send_hook(&format!("/flow/webhook/{}", token), &body, "127.0.0.1")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status": "error", "error": "OpenAlgo is locked, so this alert was not acted on. Alerts are refused after OpenAlgo starts and after you log out: sign in to OpenAlgo to receive them again."})
        )
    );
    let (s, _, v) = f
        .send_hook("/flow/webhook/not-a-known-token", &body, "127.0.0.1")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Invalid webhook token"})
        )
    );
}

/// A webhook through the tunnel: a loopback peer with a forwarding header.
fn tunneled(path: &str) -> Request<Body> {
    let mut r = hook(path, "{}", Some("application/json"), "127.0.0.1");
    r.headers_mut()
        .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    r
}

/// T-109: tunnel callers share one identity, so the per-address budget is
/// not charged for them (a hundred calls over two tokens leave a third
/// token answered, where one network address is spent: public.rs
/// one_address_cannot_buy_budget_by_changing_tokens); each token keeps its
/// own cap, its hundredth call answered and the next refused.
#[tokio::test]
async fn tunnel_callers_are_capped_per_token_not_per_address() {
    let f = F::new();
    let mut urls = Vec::new();
    for name in ["A", "B", "C"] {
        let (id, token, _) = f.live_webhook(name).await;
        f.post(&format!("/flow/api/workflows/{}/webhook/disable", id))
            .await;
        urls.push(format!("/flow/webhook/{}", token));
    }
    for i in 0..100 {
        let (s, _, _) = f.h.send(tunneled(&urls[i % 2])).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "call {}", i);
    }
    let (s, _, _) = f.h.send(tunneled(&urls[2])).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "no address budget behind the tunnel"
    );
    for i in 50..100 {
        let (s, _, _) = f.h.send(tunneled(&urls[0])).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "token A call {}", i);
    }
    let (s, h, _) = f.h.send(tunneled(&urls[0])).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "token A is spent");
    assert_eq!(h.get(header::RETRY_AFTER).unwrap(), "60");
}

/// T-109 (decision 21): the body cap's edge: an alert of exactly 2 MiB is
/// read and runs; one byte more is refused before it is read.
#[tokio::test]
async fn an_alert_of_exactly_the_body_cap_is_read() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("At the cap").await;
    let cap = 2 * 1024 * 1024;
    let frame = json!({"secret": secret, "note": ""}).to_string();
    let body = |extra: usize| {
        let filler = "x".repeat(cap - frame.len() + extra);
        let b = json!({"secret": secret, "note": filler}).to_string();
        assert_eq!(b.len(), cap + extra);
        b
    };
    let url = format!("/flow/webhook/{}", token);
    let (s, _, _) = f.send_hook(&url, &body(1), "127.0.0.1").await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    let (s, _, v) = f.send_hook(&url, &body(0), "127.0.0.1").await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (_, runs) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/executions", id),
            None,
        )
        .await;
    assert_eq!(
        runs.as_array().map(Vec::len),
        Some(1),
        "one run, at the cap"
    );
}
