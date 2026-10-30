//! Flow's trigger routes over HTTP, against the web's `blueprints/flow.py`
//! (parity R-04, R-09..R-11, R-24..R-57, T-070..T-093, X-143..X-152,
//! X-327, X-395, D-06, D-08..D-12): activation, deactivation, the webhook
//! settings, the two public webhooks (their credential, payload, rate
//! limits, lockout), and the monitors' status.

use super::*;
use crate::server::routes::Access;

struct F {
    h: H,
    cookie: String,
    csrf: String,
}

const LAN_A: &str = "192.168.1.61";

fn port() -> u16 {
    if crate::config::dev_ports_enabled() {
        crate::config::DEV_HTTP_PORT
    } else {
        crate::config::DEFAULT_HTTP_PORT
    }
}

/// A public webhook request from `ip`, declared `content_type`.
fn hook(path: &str, body: &str, content_type: Option<&str>, ip: &str) -> Request<Body> {
    let mut b = Request::builder().method(Method::POST).uri(path);
    if let Some(ct) = content_type {
        b = b.header(header::CONTENT_TYPE, ct);
    }
    let mut r = b.body(Body::from(body.to_string())).unwrap();
    r.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
            ip.parse().unwrap(),
            50000,
        )));
    r.headers_mut().insert(
        header::HOST,
        format!("127.0.0.1:{}", port()).parse().unwrap(),
    );
    r
}

fn webhook_graph(name: &str) -> Value {
    json!({
        "name": name,
        "nodes": [
            {"id": "w", "type": "webhookTrigger", "position": {"x": 0, "y": 0}, "data": {"label": "TV"}},
            {"id": "l", "type": "log", "position": {"x": 0, "y": 100},
             "data": {"message": "Got {{webhook.symbol}} x{{webhook.qty}} secret={{webhook.secret}}"}}
        ],
        "edges": [{"id": "e", "source": "w", "target": "l"}]
    })
}

impl F {
    fn new() -> Self {
        let h = H::new();
        h.setup();
        h.connect_broker();
        let (cookie, csrf) = h.session(true);
        F { h, cookie, csrf }
    }

    async fn call(&self, method: Method, path: &str, body: Option<&str>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path);
        if body.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        let req = b.body(Body::from(body.unwrap_or("").to_string())).unwrap();
        self.h
            .json(with_session(req, &self.cookie, Some(&self.csrf)))
            .await
    }

    async fn post(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::POST, path, None).await
    }

    async fn create(&self, graph: Value) -> i64 {
        let (s, v) = self
            .call(
                Method::POST,
                "/flow/api/workflows",
                Some(&graph.to_string()),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{}", v);
        v["id"].as_i64().unwrap()
    }

    /// A webhook workflow, enabled and active: its token and secret.
    async fn live_webhook(&self, name: &str) -> (i64, String, String) {
        let id = self.create(webhook_graph(name)).await;
        let (s, v) = self
            .post(&format!("/flow/api/workflows/{}/webhook/enable", id))
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v);
        let (s, v2) = self
            .post(&format!("/flow/api/workflows/{}/activate", id))
            .await;
        assert_eq!(s, StatusCode::OK, "{}", v2);
        (
            id,
            v["webhook_token"].as_str().unwrap().to_string(),
            v["webhook_secret"].as_str().unwrap().to_string(),
        )
    }

    async fn send_hook(
        &self,
        path: &str,
        body: &str,
        ip: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let (s, h, b) = self
            .h
            .send(hook(path, body, Some("application/json"), ip))
            .await;
        (s, h, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn last_run_logs(&self, id: i64) -> Vec<String> {
        let (_, runs) = self
            .call(
                Method::GET,
                &format!("/flow/api/workflows/{}/executions?limit=1", id),
                None,
            )
            .await;
        runs[0]["logs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["message"].as_str().unwrap().to_string())
            .collect()
    }
}

/// R-01 (trigger part), R-04, R-09, D-08, X-152, X-395: the trigger
/// routes are the web's and need the user; only the two webhooks are
/// public, CSRF-exempt and rate limited.
#[tokio::test]
async fn the_trigger_routes_need_the_user_and_only_the_webhooks_are_public() {
    let table = crate::server::routes::table();
    let flow: Vec<(String, &str, Access)> = table
        .iter()
        .filter(|r| r.path.starts_with("/flow"))
        .map(|r| (r.method.to_string(), r.path, r.access))
        .collect();
    for (m, p) in [
        ("POST", "/flow/api/workflows/{id}/activate"),
        ("POST", "/flow/api/workflows/{id}/deactivate"),
        ("GET", "/flow/api/workflows/{id}/webhook"),
        ("POST", "/flow/api/workflows/{id}/webhook/enable"),
        ("POST", "/flow/api/workflows/{id}/webhook/disable"),
        ("POST", "/flow/api/workflows/{id}/webhook/regenerate"),
        ("POST", "/flow/api/workflows/{id}/webhook/regenerate-secret"),
        ("POST", "/flow/api/workflows/{id}/webhook/auth-type"),
        ("GET", "/flow/api/monitor/status"),
    ] {
        assert!(
            flow.contains(&(m.to_string(), p, Access::User)),
            "{} {}",
            m,
            p
        );
    }
    let public: Vec<(&str, &str)> = flow
        .iter()
        .filter(|(_, _, a)| *a == Access::Public)
        .map(|(m, p, _)| (m.as_str(), *p))
        .collect();
    assert_eq!(
        public,
        vec![
            ("POST", "/flow/webhook/{token}"),
            ("POST", "/flow/webhook/{token}/{symbol}")
        ]
    );
    let f = F::new();
    let (s, _) = f.h.json(get("/flow/api/monitor/status")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // A session write without its CSRF token is refused; the webhook,
    // which has neither, reaches its handler.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/flow/api/workflows/1/activate")
        .body(Body::empty())
        .unwrap();
    let (s, _) = f.h.json(with_session(req, &f.cookie, None)).await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "the session layer's CSRF refusal"
    );
    let (s, _, v) = f
        .send_hook("/flow/webhook/not-a-token", "{}", "127.0.0.1")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Invalid webhook token"})
        )
    );
}

/// R-25, R-26, R-27, R-32, T-004, T-081, T-084, V-133: activation's
/// guards in the web's order; a schedule that cannot register rolls the
/// activation back; deactivation, already-inactive, 404 and 409.
#[tokio::test]
async fn activation_and_deactivation_follow_the_webs_guards() {
    let f = F::new();
    let (s, v) = f.post("/flow/api/workflows/999/activate").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Workflow not found"})
        )
    );
    let empty = f.create(json!({"name": "Empty"})).await;
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/activate", empty))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "Workflow cannot be activated");
    assert_eq!(v["status"], "error");
    assert!(v["errors"].as_array().is_some_and(|e| !e.is_empty()));

    let id = f.create(webhook_graph("Hook")).await;
    let path = format!("/flow/api/workflows/{}/activate", id);
    let (s, v) = f.post(&path).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Workflow activated with webhookTrigger trigger"})
        )
    );
    let (_, v) = f.post(&path).await;
    assert_eq!(
        v,
        json!({"status": "already_active", "message": "Workflow is already active"})
    );

    let mut daily = webhook_graph("Daily");
    daily["nodes"][0] = json!({"id": "s", "type": "start", "position": {"x": 0, "y": 0},
        "data": {"scheduleType": "yearly", "time": "09:15"}});
    daily["edges"][0]["source"] = json!("s");
    let bad = f.create(daily).await;
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/activate", bad))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::BAD_REQUEST,
            json!({"error": "Invalid schedule configuration: type=yearly"})
        )
    );
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", bad), None)
        .await;
    assert_eq!(w["is_active"], false, "rolled back");

    let deact = format!("/flow/api/workflows/{}/deactivate", id);
    let claim = f.h.ctx().flow.try_claim(id);
    let (s, v) = f.post(&deact).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::CONFLICT,
            json!({"error": "This workflow is still running. Try again when it finishes."})
        )
    );
    drop(claim);
    let (s, v) = f.post(&deact).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Workflow deactivated"})
        )
    );
    let (_, v) = f.post(&deact).await;
    assert_eq!(
        v,
        json!({"status": "already_inactive", "message": "Workflow is already inactive"})
    );
    let (s, _) = f.post("/flow/api/workflows/999/deactivate").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// R-27, R-32, S-21, T-082, T-084: a schedule's job id is recorded at
/// activation and cleared, with the job, at deactivation.
#[tokio::test]
async fn a_schedule_is_registered_and_removed_with_its_workflow() {
    let f = F::new();
    let mut g = webhook_graph("Daily");
    g["nodes"][0] = json!({"id": "s", "type": "start", "position": {"x": 0, "y": 0},
        "data": {"scheduleType": "daily", "time": "09:15", "marketHoursOnly": true}});
    g["edges"][0]["source"] = json!("s");
    let id = f.create(g).await;
    let (_, v) = f
        .post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    assert_eq!(v["message"], "Workflow activated with start trigger");
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(w["schedule_job_id"], format!("flow_workflow_{}", id));
    f.post(&format!("/flow/api/workflows/{}/deactivate", id))
        .await;
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(
        (w["is_active"].clone(), w["schedule_job_id"].clone()),
        (json!(false), Value::Null)
    );
}

/// R-39..R-41, R-46, R-49..R-56, T-070..T-076, X-327: the webhook runs its
/// workflow with the payload under `{{webhook}}`, the payload-mode secret
/// popped first, the path's symbol over the body's; the reply is the web's.
#[tokio::test]
async fn a_webhook_runs_its_workflow_with_the_payload() {
    let f = F::new();
    let id = f.create(webhook_graph("TV long")).await;
    let (_, info) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/webhook", id),
            None,
        )
        .await;
    let token = info["webhook_token"].as_str().unwrap().to_string();
    let secret = info["webhook_secret"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 43);
    assert_eq!(secret.len(), 64);
    let base = format!("http://127.0.0.1:{}", f.h.ctx().listening_port());
    assert_eq!(
        info["webhook_url"],
        format!("{}/flow/webhook/{}", base, token)
    );
    assert_eq!(
        info["webhook_url_with_symbol"],
        format!("{}/flow/webhook/{}/{{symbol}}", base, token)
    );
    assert_eq!(
        (
            info["webhook_auth_type"].clone(),
            info["webhook_url_with_secret"].clone()
        ),
        (json!("payload"), Value::Null)
    );
    let url = format!("/flow/webhook/{}", token);
    let body = json!({"secret": secret, "symbol": "SBIN", "qty": 5}).to_string();
    let (s, _, v) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "Webhook is disabled"})
        )
    );
    f.post(&format!("/flow/api/workflows/{}/webhook/enable", id))
        .await;
    let (s, _, v) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "Workflow is not active"})
        )
    );
    f.post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    let (s, _, v) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    assert_eq!(v["message"], "Workflow 'TV long' triggered");
    assert_eq!(
        (v["errors"].clone(), v["workflow_id"].clone()),
        (Value::Null, json!(id))
    );
    assert!(v["execution_id"].as_i64().is_some());
    let logs = f.last_run_logs(id).await;
    assert!(
        logs.contains(&"[LOG] Got SBIN x5 secret={{webhook.secret}}".to_string()),
        "{:?}",
        logs
    );
    assert!(logs.contains(&"Webhook trigger activated".to_string()));
    let (s, _, _) = f
        .send_hook(&format!("{}/INFY", url), &body, "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(f
        .last_run_logs(id)
        .await
        .contains(&"[LOG] Got INFY x5 secret={{webhook.secret}}".to_string()));
}

/// R-52, T-072 and the WEB-DEFECTs: a missing secret, a wrong one, one
/// that is not text (500 on the web), plain text in payload mode, URL auth
/// with and without `?secret=`.
#[tokio::test]
async fn the_secret_is_required_as_the_auth_type_says() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("Guarded").await;
    let url = format!("/flow/webhook/{}", token);
    let (s, _, v) = f
        .send_hook(&url, r#"{"symbol": "SBIN"}"#, "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .starts_with("Missing webhook secret in payload."));
    let (s, _, v) = f.send_hook(&url, r#"{"secret": 123}"#, "127.0.0.1").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::UNAUTHORIZED,
            json!({"error": "Invalid webhook secret"})
        )
    );
    let (s, _, _) =
        f.h.send(hook(&url, "BUY SBIN", Some("text/plain"), "127.0.0.1"))
            .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/webhook/auth-type", id),
            Some(r#"{"auth_type": "url"}"#),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["message"], "Webhook auth type set to 'url'");
    assert!(v["webhook_url_with_secret"]
        .as_str()
        .unwrap()
        .ends_with(&format!("?secret={}", secret)));
    let (s, _, v) = f
        .send_hook(&url, r#"{"symbol": "SBIN"}"#, "127.0.0.1")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::UNAUTHORIZED,
            json!({"error": "Missing webhook secret in URL. Use ?secret=your_secret"})
        )
    );
    let (s, _, _) =
        f.h.send(hook(
            &format!("{}?secret={}", url, secret),
            "BUY SBIN",
            Some("text/plain"),
            "127.0.0.1",
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "URL auth carries plain text");
}

/// R-41..R-45 (WEB-DEFECTs: an unknown workflow answered 500 or 400, a
/// missing body 500), S-17, S-18: the settings routes, and rotation that
/// takes effect on the very next request.
#[tokio::test]
async fn the_settings_routes_rotate_and_answer_404_for_an_unknown_workflow() {
    let f = F::new();
    for p in ["webhook/enable", "webhook/disable", "webhook/auth-type"] {
        let (s, v) = f.post(&format!("/flow/api/workflows/999/{}", p)).await;
        assert_eq!(
            (s, v),
            (
                StatusCode::NOT_FOUND,
                json!({"error": "Workflow not found"})
            ),
            "{}",
            p
        );
    }
    let (s, v) = f.post("/flow/api/workflows/999/webhook/regenerate").await;
    assert_eq!(
        (s, v),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to regenerate token"})
        )
    );
    let (id, token, secret) = f.live_webhook("Rotating").await;
    let auth = format!("/flow/api/workflows/{}/webhook/auth-type", id);
    let (s, v) = f
        .call(Method::POST, &auth, Some(r#"{"auth_type": "header"}"#))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::BAD_REQUEST,
            json!({"error": "Invalid auth type"})
        )
    );
    let (s, v) = f.post(&auth).await;
    assert_eq!(
        (s, v["webhook_auth_type"].clone()),
        (StatusCode::OK, json!("payload")),
        "no body is the default"
    );
    let (_, v) = f
        .post(&format!(
            "/flow/api/workflows/{}/webhook/regenerate-secret",
            id
        ))
        .await;
    assert_eq!(v["message"], "Webhook secret regenerated");
    let fresh = v["webhook_secret"].as_str().unwrap().to_string();
    assert_ne!(fresh, secret);
    let url = format!("/flow/webhook/{}", token);
    let (s, _, _) = f
        .send_hook(&url, &json!({"secret": secret}).to_string(), "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "the old secret stops at once");
    let (s, _, _) = f
        .send_hook(&url, &json!({"secret": fresh}).to_string(), "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = f
        .post(&format!("/flow/api/workflows/{}/webhook/regenerate", id))
        .await;
    assert_eq!(v["message"], "Webhook token and secret regenerated");
    let (s, _, _) = f
        .send_hook(
            &url,
            &json!({"secret": v["webhook_secret"]}).to_string(),
            "127.0.0.1",
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "the old address stops at once");
    let (s, _, _) = f
        .send_hook(
            &format!("/flow/webhook/{}", v["webhook_token"].as_str().unwrap()),
            &json!({"secret": v["webhook_secret"]}).to_string(),
            "127.0.0.1",
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/webhook/disable", id))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Webhook disabled"})
        )
    );
}

/// Decisions.md 13: ten wrong secrets lock the webhook (403 even with the
/// right one); a new secret unlocks it.
#[tokio::test]
async fn wrong_secrets_lock_the_webhook() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("Locked").await;
    let url = format!("/flow/webhook/{}", token);
    for i in 0..10 {
        let (s, _, _) = f
            .send_hook(
                &url,
                &json!({"secret": format!("guess{}", i)}).to_string(),
                "127.0.0.1",
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, _, v) = f
        .send_hook(&url, &json!({"secret": secret}).to_string(), "127.0.0.1")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::FORBIDDEN,
            json!({"error": crate::flow::service::webhook_guard::LOCKED})
        )
    );
    let (_, v) = f
        .post(&format!(
            "/flow/api/workflows/{}/webhook/regenerate-secret",
            id
        ))
        .await;
    let (s, _, _) = f
        .send_hook(
            &url,
            &json!({"secret": v["webhook_secret"]}).to_string(),
            "127.0.0.1",
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

/// D-06, lead decision 16: with no broker logged in the webhook still
/// runs its workflow, as the strategy and Chartink webhooks do: a node
/// that needs no broker runs, a broker call fails at its node with the
/// no-broker-session sentence (not the web's "Invalid openalgo apikey")
/// and stops its branch. Signed out, the webhook answers the locked 503.
#[tokio::test]
async fn a_webhook_without_a_broker_session_runs_and_says_why_a_node_failed() {
    let f = F::new();
    let mut g = webhook_graph("No broker");
    g["nodes"][1]["data"]["message"] = json!("before the quote");
    g["nodes"].as_array_mut().unwrap().push(
        json!({"id": "q", "type": "subscribeLtp", "position": {"x": 0, "y": 200},
            "data": {"symbol": "SBIN", "exchange": "NSE", "outputVariable": "ltp"}}),
    );
    g["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "e2", "source": "l", "target": "q"}));
    let id = f.create(g).await;
    f.post(&format!("/flow/api/workflows/{}/webhook/enable", id))
        .await;
    let (_, info) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/webhook", id),
            None,
        )
        .await;
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let url = format!("/flow/webhook/{}", info["webhook_token"].as_str().unwrap());
    let secret = info["webhook_secret"].as_str().unwrap().to_string();
    f.h.ctx().set_broker_session(None);
    let (s, _, v) = f
        .send_hook(&url, &json!({"secret": secret}).to_string(), "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{}", v);
    let sentence = crate::strategy::dispatch::NO_BROKER_SESSION;
    assert_eq!(v["errors"][0]["message"], sentence);
    let logs = f.last_run_logs(id).await;
    assert!(
        logs.contains(&"[LOG] before the quote".to_string()),
        "{:?}",
        logs
    );
    assert!(
        logs.contains(&format!(
            "Stopping this branch: subscribeLtp node failed ({})",
            sentence
        )),
        "{:?}",
        logs
    );
    f.h.ctx().security.lock_with(|| {});
    let (s, _, v) = f
        .send_hook(&url, &json!({"secret": secret}).to_string(), "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"], crate::services::core::WEBHOOK_LOCKED);
}

/// R-10, R-11, T-077, T-109, D-10, D-11, X-143..X-148, X-150: 100 a minute
/// per token, both spellings on one budget, from any address; over it the
/// web's JSON 429 with `Retry-After`, never a redirect.
#[tokio::test]
async fn webhooks_are_rate_limited_per_caller_and_per_token() {
    let f = F::new();
    let (id, token, _) = f.live_webhook("Busy").await;
    // Disabled: every call is admitted and answered 403 without a run.
    f.post(&format!("/flow/api/workflows/{}/webhook/disable", id))
        .await;
    let url = format!("/flow/webhook/{}", token);
    for i in 0..100 {
        let path = if i % 2 == 0 {
            url.clone()
        } else {
            format!("{}/SBIN", url)
        };
        let ip = ["192.168.1.61", "192.168.1.62", "192.168.1.63"][i % 3];
        let (s, _, _) = f.send_hook(&path, "{}", ip).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "call {}", i);
    }
    let (s, h, v) = f
        .send_hook(&format!("{}/RELIANCE", url), "{}", "192.168.1.64")
        .await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "the token is capped from a fresh address too"
    );
    assert_eq!(
        v,
        json!({"status": "error", "message": "Rate limit exceeded. Please slow down your requests.", "limit": "100 per 1 minute", "retry_after": 60})
    );
    assert_eq!(h.get(header::RETRY_AFTER).unwrap(), "60");
    assert!(h.get(header::LOCATION).is_none());
    // Another workflow keeps its own budget.
    let (other, other_token, _) = f.live_webhook("Other").await;
    f.post(&format!("/flow/api/workflows/{}/webhook/disable", other))
        .await;
    let (s, _, _) = f
        .send_hook(
            &format!("/flow/webhook/{}", other_token),
            "{}",
            "192.168.1.65",
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The per-address budget across tokens: public.rs
    // one_address_cannot_buy_budget_by_changing_tokens.
}

/// D-10: an unknown token counts against the caller's failure budget; once
/// spent, its guesses are refused early.
#[tokio::test]
async fn guessing_tokens_spends_the_callers_failure_budget() {
    let f = F::new();
    for i in 0..10 {
        let (s, _, _) = f
            .send_hook(&format!("/flow/webhook/guess{}", i), "{}", LAN_A)
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let (s, _, _) = f.send_hook("/flow/webhook/guess-more", "{}", LAN_A).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

/// T-093, R-57: the monitors' status over HTTP.
#[tokio::test]
async fn the_monitor_status_is_the_webs() {
    let f = F::new();
    let (s, v) = f.call(Method::GET, "/flow/api/monitor/status", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["poll_interval"], 5);
    assert_eq!(
        v["order_updates"],
        json!({"watches_count": 0, "watches": []})
    );
}

/// R-21, T-085: saving an active workflow with a changed trigger
/// re-registers it with the new settings; a trigger that can no longer be
/// registered stands the workflow down and says so (`needs_reactivate`).
#[tokio::test]
async fn a_saved_trigger_change_re_registers_or_stands_down() {
    let f = F::new();
    let mut g = webhook_graph("Daily");
    g["nodes"][0] = json!({"id": "s", "type": "start", "position": {"x": 0, "y": 0},
        "data": {"scheduleType": "daily", "time": "09:15"}});
    g["edges"][0]["source"] = json!("s");
    let id = f.create(g.clone()).await;
    f.post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    let next = |f: &F| {
        crate::flow::triggers::store::next_run(&f.h.ctx().sqlite.conn().unwrap(), id).unwrap()
    };
    let before = next(&f);
    g["nodes"][0]["data"]["time"] = json!("10:30");
    let path = format!("/flow/api/workflows/{}", id);
    let (s, v) = f
        .call(
            Method::PUT,
            &path,
            Some(&json!({"nodes": g["nodes"]}).to_string()),
        )
        .await;
    assert_eq!(
        (s, v["needs_reactivate"].clone(), v["is_active"].clone()),
        (StatusCode::OK, json!(false), json!(true))
    );
    assert_ne!(next(&f), before, "the job follows the new time");
    g["nodes"][0]["data"]["scheduleType"] = json!("yearly");
    let (_, v) = f
        .call(
            Method::PUT,
            &path,
            Some(&json!({"nodes": g["nodes"]}).to_string()),
        )
        .await;
    assert_eq!(
        (v["needs_reactivate"].clone(), v["is_active"].clone()),
        (json!(true), json!(false))
    );
}

/// Trigger hooks whose feed refuses to give a subscription back while
/// `refuse` is set, recording every unregister.
#[derive(Default)]
struct StuckFeed {
    refuse: std::sync::atomic::AtomicBool,
    unregistered: parking_lot::Mutex<Vec<i64>>,
}

#[async_trait::async_trait]
impl crate::flow::contract::subscriptions::FeedRelease for StuckFeed {
    async fn unsubscribe(&self, _: &str, _: &str, _: &str) -> bool {
        !self.refuse.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::flow::contract::triggers::TriggerHooks for StuckFeed {
    async fn register(
        &self,
        _: i64,
        _: &str,
        _: &serde_json::Map<String, Value>,
    ) -> Result<(), crate::flow::contract::triggers::TriggerError> {
        Ok(())
    }

    async fn unregister(&self, id: i64) -> Result<(), String> {
        self.unregistered.lock().push(id);
        Ok(())
    }
}

/// R-32, N-stream-06, T-084, X-320, X-321: while the feed has not given a
/// workflow's subscription back, deactivation answers 503 and leaves its
/// triggers and its active flag alone; the retry succeeds and releases it.
/// An inactive workflow still holding one answers 503 the same way.
#[tokio::test]
async fn deactivation_waits_for_the_feed_to_give_back_its_subscriptions() {
    let f = F::new();
    let stuck = std::sync::Arc::new(StuckFeed::default());
    f.h.ctx().flow.set_triggers(stuck.clone());
    let id = f.create(webhook_graph("Streaming")).await;
    f.post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    let subs = &f.h.ctx().flow.subscriptions;
    subs.record(Some(id), "SBIN", "NSE", "LTP");
    stuck
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let deact = format!("/flow/api/workflows/{}/deactivate", id);
    let (s, v) = f.post(&deact).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "The workflow feed is still connected. Try again shortly."})
        )
    );
    assert!(
        stuck.unregistered.lock().is_empty(),
        "triggers kept for the retry"
    );
    assert!(subs.has(id));
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(w["is_active"], true);
    stuck
        .refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (s, _) = f.post(&deact).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(*stuck.unregistered.lock(), vec![id]);
    assert!(!subs.has(id));
    // Already inactive, still holding one: 503 until the feed lets go.
    subs.record(Some(id), "TCS", "NSE", "Quote");
    stuck
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, _) = f.post(&deact).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    stuck
        .refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (s, v) = f.post(&deact).await;
    assert_eq!(
        (s, v["status"].clone()),
        (StatusCode::OK, json!("already_inactive"))
    );
    assert!(!subs.has(id));
}

/// A price-alert workflow, active, with the trigger side's alert: its id,
/// the side and the alert as the poll latched it.
async fn alert_workflow(
    f: &F,
    trigger: &str,
) -> (
    i64,
    std::sync::Arc<crate::flow::triggers::TriggerSide>,
    crate::flow::triggers::price_monitor::Alert,
) {
    let id = f
        .create(json!({
            "name": "Breakout",
            "nodes": [
                {"id": "p", "type": "priceAlert", "position": {"x": 0, "y": 0},
                 "data": {"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800, "trigger": trigger}},
                {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": "Bought at {{webhook.trigger_price}}"}}
            ],
            "edges": [{"id": "e", "source": "p", "target": "l", "sourceHandle": "true"}]
        }))
        .await;
    let ctx = f.h.ctx();
    crate::flow::store::workflows::set_active(&ctx.sqlite.conn().unwrap(), id, true, ctx.now())
        .unwrap();
    let side =
        crate::flow::triggers::TriggerSide::new(std::sync::Arc::downgrade(ctx), ctx.clock.clone());
    let data = json!({"symbol": "SBIN", "condition": "above", "price": 800, "trigger": trigger});
    let gen = side.alerts.add(
        crate::flow::triggers::price_monitor::Alert::from_node(
            id,
            data.as_object().unwrap(),
            ctx.now(),
        )
        .unwrap(),
    );
    let crate::flow::triggers::price_monitor::Observed::Fire(a) =
        side.alerts.observe(id, gen, 812.75)
    else {
        panic!("the alert should fire");
    };
    (id, side, *a)
}

/// Waits (bounded) until the side has no run pending for the workflow.
async fn settled(side: &crate::flow::triggers::TriggerSide, id: i64) {
    for _ in 0..500 {
        if side.alerts.claim_pending(id) {
            side.alerts.release_pending(id);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the price-alert run did not finish");
}

/// X-154, X-155, T-040..T-042, N-priceAlert-03: the price monitor's run
/// trusts the tick that fired it (no second quote) and takes the true
/// branch; a one-shot alert is then retired and its workflow deactivated.
/// X-156, T-036: an every-time alert stays. X-218: a run that met another
/// run of the workflow did not execute, so the one-shot stays armed.
#[tokio::test]
async fn a_price_alert_run_trusts_its_tick_and_retires_a_one_shot() {
    let f = F::new();
    let (id, side, alert) = alert_workflow(&f, "once").await;
    let claim = f.h.ctx().flow.try_claim(id);
    side.queue_price_run(alert.clone(), 812.75);
    settled(&side, id).await;
    drop(claim);
    let a = side
        .alerts
        .get(id)
        .expect("already running: still registered");
    assert!(!a.triggered, "and re-armed");
    let crate::flow::triggers::price_monitor::Observed::Fire(a) =
        side.alerts.observe(id, a.gen, 812.75)
    else {
        panic!("re-armed alert fires again");
    };
    side.queue_price_run(*a, 812.75);
    settled(&side, id).await;
    assert!(side.alerts.get(id).is_none(), "a one-shot is spent");
    let logs = f.last_run_logs(id).await;
    assert!(
        logs.contains(
            &"Price alert fired by monitor: SBIN LTP=812.75 greater_than 800.0".to_string()
        ),
        "{:?}",
        logs
    );
    assert!(
        logs.contains(&"[LOG] Bought at 812.75".to_string()),
        "{:?}",
        logs
    );
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(w["is_active"], false);

    let (id, side, alert) = alert_workflow(&f, "every_time").await;
    side.queue_price_run(alert, 812.75);
    settled(&side, id).await;
    assert!(side.alerts.get(id).is_some(), "every time: still watching");
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(w["is_active"], true);
}

#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

#[cfg(not(unix))]
fn open_fds() -> usize {
    0
}

fn rss_kb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Descriptors less two per pooled database connection, once the pool and
/// the count agree twice in a row (r2d2 opens connections on its own
/// thread).
async fn settled_fds(ctx: &AppState) -> usize {
    let mut last = None;
    for _ in 0..250 {
        let pool = ctx.sqlite.pool_state().0 as usize;
        let now = open_fds().saturating_sub(2 * pool);
        if last == Some(now) {
            return now;
        }
        last = Some(now);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("descriptors never settled");
}

/// A workflow's finished runs.
fn finished_runs(ctx: &AppState, id: i64) -> i64 {
    ctx.sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM flow_workflow_executions WHERE workflow_id = ?1 AND status != 'running'",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// Fire one run of the trigger the leak test gave workflow `kind` (0 the
/// schedule, 1 the price alert, 2 the order watch from the bus; the
/// webhook workflow fires nothing), and wait (bounded) until it finished,
/// its side settled and the workflow is free again.
async fn fire_and_settle(
    f: &F,
    side: &std::sync::Arc<crate::flow::triggers::TriggerSide>,
    kind: usize,
    id: i64,
) {
    use crate::flow::triggers::price_monitor::Observed;
    let ctx = f.h.ctx();
    let before = finished_runs(ctx, id);
    match kind {
        0 => side.scheduled_run(id, false).await,
        1 => {
            let gen = side.alerts.get(id).unwrap().gen;
            let Observed::Fire(fired) = side.alerts.observe(id, gen, 2e9) else {
                panic!("the alert fires above its price");
            };
            side.queue_price_run(*fired, 2e9);
        }
        2 => {
            ctx.bus.publish(crate::events::Event::OrderUpdate(
                crate::events::OrderUpdate {
                    orderid: format!("leak{}", before),
                    symbol: "SBIN".into(),
                    exchange: "NSE".into(),
                    order_status: "complete".into(),
                    ..Default::default()
                },
            ));
        }
        _ => return,
    }
    for _ in 0..500 {
        let done = finished_runs(ctx, id) > before
            && side.watches.get(id).is_none_or(|w| !w.triggered)
            && side.alerts.claim_pending(id);
        if done {
            side.alerts.release_pending(id);
            if ctx.flow.try_claim(id).is_some() {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the run of workflow {} never settled", id);
}

/// fd-audit (T-111, X-346, CLAUDE.md "Measure, do not just read"):
/// activating and deactivating a schedule, a price alert, an order-update
/// watch and a webhook workflow 100 times, with a feed subscription given
/// back each time and, in every cycle, a run fired by each of the first
/// three (the schedule's, the alert's, and the order update's through the
/// bus), leaves descriptors, tasks, registrations and memory flat. CI runs
/// it alone in its own step (decision 27, ci.yml "Flow triggers leak
/// nothing"); by hand: `cargo test --lib flow_triggers::triggers_do_not_leak
/// -- --ignored --nocapture --test-threads=1`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measures the process: run alone"]
async fn triggers_do_not_leak() {
    let f = F::new();
    let ctx = f.h.ctx().clone();
    // A side this test holds stands in for the app's own (stopped first,
    // so its start-up restore cannot race the saves), so each kind's run
    // can be fired: its scheduler driver and price poll run as the app's
    // do, and it hears order updates from the bus.
    ctx.flow.triggers().stop().await;
    let side =
        crate::flow::triggers::TriggerSide::new(std::sync::Arc::downgrade(&ctx), ctx.clock.clone());
    ctx.flow.set_triggers(side.clone());
    crate::flow::triggers::drive::subscribe_order_updates(&ctx, &side);
    side.watches.attach();
    side.start_drivers();
    let mut g = webhook_graph("Every minute");
    g["nodes"][0] = json!({"id": "s", "type": "start", "position": {"x": 0, "y": 0},
        "data": {"scheduleType": "interval", "intervalValue": 1, "intervalUnit": "minutes"}});
    g["edges"][0]["source"] = json!("s");
    let mut a = webhook_graph("Alert");
    a["nodes"][0] = json!({"id": "p", "type": "priceAlert", "position": {"x": 0, "y": 0},
        "data": {"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 1e9,
            "trigger": "every_time"}});
    a["edges"][0]["source"] = json!("p");
    a["edges"][0]["sourceHandle"] = json!("true");
    let mut o = webhook_graph("Fills");
    o["nodes"][0] = json!({"id": "o", "type": "orderUpdateTrigger", "position": {"x": 0, "y": 0},
        "data": {"symbol": "SBIN", "status": "complete", "trigger": "every_time"}});
    o["edges"][0]["source"] = json!("o");
    let ids = vec![
        f.create(g).await,
        f.create(a).await,
        f.create(o).await,
        f.create(webhook_graph("Hook")).await,
    ];
    let metrics = tokio::runtime::Handle::current().metrics();
    let cycle = |n: usize| {
        let f = &f;
        let ids = ids.clone();
        let ctx = ctx.clone();
        let side = side.clone();
        async move {
            for _ in 0..n {
                for (kind, id) in ids.iter().enumerate() {
                    let (s, v) = f
                        .post(&format!("/flow/api/workflows/{}/activate", id))
                        .await;
                    assert_eq!(s, StatusCode::OK, "{}", v);
                    ctx.flow
                        .subscriptions
                        .record(Some(*id), "SBIN", "NSE", "LTP");
                    fire_and_settle(f, &side, kind, *id).await;
                    let (s, v) = f
                        .post(&format!("/flow/api/workflows/{}/deactivate", id))
                        .await;
                    assert_eq!(s, StatusCode::OK, "{}", v);
                }
            }
        }
    };
    cycle(5).await;
    let (fds0, rss0, tasks0) = (settled_fds(&ctx).await, rss_kb(), metrics.num_alive_tasks());
    cycle(100).await;
    let (fds1, rss1, tasks1) = (settled_fds(&ctx).await, rss_kb(), metrics.num_alive_tasks());
    let fired: Vec<i64> = ids[..3].iter().map(|id| finished_runs(&ctx, *id)).collect();
    eprintln!(
        "flow triggers, 100 x 4 activate/deactivate with a schedule, alert and order-update run each cycle (runs {:?}): descriptors {} -> {}, RSS {} -> {} KiB, tasks {} -> {}",
        fired, fds0, fds1, rss0, rss1, tasks0, tasks1
    );
    assert_eq!(fired, vec![105, 105, 105], "every cycle ran each kind once");
    assert!(fds1 <= fds0 + 2, "descriptors grew: {} -> {}", fds0, fds1);
    assert!(tasks1 <= tasks0 + 1, "tasks grew: {} -> {}", tasks0, tasks1);
    assert_eq!(
        side.task_count(),
        2,
        "only the driver and the poll are left"
    );
    assert!(
        rss1 < rss0 + 20 * 1024,
        "RSS grew: {} -> {} KiB",
        rss0,
        rss1
    );
    let status = ctx.flow.triggers().status();
    assert_eq!(
        (
            status["alerts_count"].clone(),
            status["order_updates"]["watches_count"].clone()
        ),
        (json!(0), json!(0))
    );
    for id in &ids {
        assert!(!ctx.flow.subscriptions.has(*id));
    }
    crate::flow::contract::triggers::TriggerHooks::stop(side.as_ref()).await;
}

/// X-324 (decision 2): a webhook to a workflow that waits is answered 202
/// at once, the message saying it finishes in the background; a second
/// trigger meanwhile is refused 409; the workflow is free once the run
/// ends.
#[tokio::test]
async fn a_webhook_to_a_waiting_workflow_is_answered_at_once() {
    let f = F::new();
    let mut g = webhook_graph("Waits");
    g["nodes"][1] = json!({"id": "l", "type": "delay", "position": {"x": 0, "y": 100},
        "data": {"delayValue": 60}});
    let id = f.create(g).await;
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/webhook/enable", id))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let (s, active) = f
        .post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", active);
    let url = format!("/flow/webhook/{}", v["webhook_token"].as_str().unwrap());
    let body = json!({"secret": v["webhook_secret"]}).to_string();
    let (s, _, first) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(s, StatusCode::ACCEPTED, "{}", first);
    assert_eq!(first["status"], "accepted");
    assert!(
        first["message"].as_str().unwrap().contains("background"),
        "{}",
        first
    );
    let (s, _, second) = f.send_hook(&url, &body, "127.0.0.1").await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", second);
    assert!(
        second["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("already running"),
        "{}",
        second
    );
    let ctx = f.h.ctx().clone();
    ctx.flow.stop_all(crate::flow::STOP_GRACE).await;
    assert!(ctx.flow.try_claim(id).is_some());
}

mod monitors;
mod public;
mod settings;
