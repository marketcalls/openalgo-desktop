//! The public webhooks' edges over HTTP: the body shapes (multipart,
//! Python JSON), the per-address budget across tokens, the body cap, the
//! resource guard, the traffic log, the settings answers, workflows whose
//! trigger is not a webhook node, an empty payload, and replace (parity
//! D-13, R-39..R-41, R-44, R-46, R-48, R-50, R-56, T-070, T-073, T-077,
//! T-085, T-107, T-109, X-149).

use super::*;

/// The workflow's settings as the session route answers them.
async fn settings(f: &F, id: i64) -> Value {
    let (s, v) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/webhook", id),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    v
}

/// R-50 (decision 24): a multipart body is read as its fields, as the web's
/// `request.form` reads it; a file part is not a field.
#[tokio::test]
async fn a_multipart_body_is_read_as_its_fields() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("Multipart").await;
    let part = |name: &str, value: &str| {
        format!(
            "--XyZ\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
            name, value
        )
    };
    let body = format!(
        "{}{}{}--XyZ\r\nContent-Disposition: form-data; name=\"qty\"; filename=\"chart.png\"\r\nContent-Type: image/png\r\n\r\nPNG\r\n--XyZ--\r\n",
        part("secret", &secret),
        part("symbol", "SBIN"),
        part("qty", "7"),
    );
    let (s, _, b) =
        f.h.send(hook(
            &format!("/flow/webhook/{}", token),
            &body,
            Some("multipart/form-data; boundary=XyZ"),
            "127.0.0.1",
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    let logs = f.last_run_logs(id).await;
    assert!(
        logs.contains(&"[LOG] Got SBIN x7 secret={{webhook.secret}}".to_string()),
        "{:?}",
        logs
    );
}

/// T-073 (decision 24): TradingView's bare `NaN` is JSON to Python, so the
/// body is a dict and its secret is found; it reads as Python prints it.
#[tokio::test]
async fn nan_from_tradingview_is_json() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("NaN").await;
    let body = format!(
        r#"{{"secret": "{}", "symbol": "SBIN", "qty": NaN}}"#,
        secret
    );
    let (s, _, v) = f
        .send_hook(&format!("/flow/webhook/{}", token), &body, "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(f
        .last_run_logs(id)
        .await
        .contains(&"[LOG] Got SBIN xnan secret={{webhook.secret}}".to_string()));
}

/// T-077, X-149: one address is limited across tokens: a hundred calls
/// spread over two tokens (each well inside its own budget) spend it, so a
/// third token's first call from that address is refused while the same
/// token from another address is still answered.
#[tokio::test]
async fn one_address_cannot_buy_budget_by_changing_tokens() {
    let f = F::new();
    let mut urls = Vec::new();
    for name in ["A", "B", "C"] {
        let (id, token, _) = f.live_webhook(name).await;
        f.post(&format!("/flow/api/workflows/{}/webhook/disable", id))
            .await;
        urls.push(format!("/flow/webhook/{}", token));
    }
    for i in 0..100 {
        let (s, _, _) = f.send_hook(&urls[i % 2], "{}", LAN_A).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "call {}", i);
    }
    let (s, _, _) = f.send_hook(&urls[2], "{}", LAN_A).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "this address is spent");
    let (s, _, _) = f.send_hook(&urls[2], "{}", "192.168.1.62").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the token is not");
}

/// T-109 (decision 21): an alert over the 2 MiB body cap is refused before
/// it is read, and nothing runs.
#[tokio::test]
async fn an_alert_over_the_body_cap_is_refused() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("Large").await;
    let filler = "x".repeat(2 * 1024 * 1024);
    let body = json!({"secret": secret, "note": filler}).to_string();
    let (s, _, _) = f
        .send_hook(&format!("/flow/webhook/{}", token), &body, "127.0.0.1")
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    let (_, runs) = f
        .call(
            Method::GET,
            &format!("/flow/api/workflows/{}/executions", id),
            None,
        )
        .await;
    assert_eq!(runs, json!([]));
}

/// T-107: over the resource guard, a Flow webhook is judged by its own
/// credential, not refused: a known token is still answered, an unknown
/// one is refused.
#[tokio::test]
async fn over_the_resource_guard_a_webhook_is_judged_by_its_token() {
    use crate::server::ratelimit::Bucket;
    let f = F::new();
    let (id, token, _) = f.live_webhook("Guarded").await;
    f.post(&format!("/flow/api/workflows/{}/webhook/disable", id))
        .await;
    let ctx = f.h.ctx();
    let now = ctx.limiter.now();
    ctx.limiter.freeze(Some(now));
    let caller = crate::server::source::Source::Lan(LAN_A.parse().unwrap()).ip();
    for _ in 0..1_000 {
        let _ = ctx.limiter.check(Bucket::Guard, caller, now);
    }
    assert!(!crate::server::middleware::within_guard(ctx, caller));
    let (s, _, v) = f
        .send_hook(&format!("/flow/webhook/{}", token), "{}", LAN_A)
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "Webhook is disabled"})
        ),
        "its handler answered"
    );
    let (s, _, _) = f.send_hook("/flow/webhook/not-a-token", "{}", LAN_A).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

/// D-13, R-48: neither the token nor a `?secret=` reaches the traffic log
/// or the 404 tracker, for a known workflow or a guessed one.
#[tokio::test]
async fn a_secret_in_the_url_never_reaches_a_log() {
    let f = F::new();
    let (id, token, secret) = f.live_webhook("Url auth").await;
    f.call(
        Method::POST,
        &format!("/flow/api/workflows/{}/webhook/auth-type", id),
        Some(r#"{"auth_type": "url"}"#),
    )
    .await;
    let guessed_secret = "guessed-secret-value-0123456789";
    for (path, want) in [
        (
            format!("/flow/webhook/{}?secret={}", token, secret),
            StatusCode::OK,
        ),
        (
            format!("/flow/webhook/{}/SBIN?secret={}", token, secret),
            StatusCode::OK,
        ),
        (
            format!("/flow/webhook/unknown-token-0123?secret={}", guessed_secret),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (s, _, _) = f.send_hook(&path, "{}", LAN_A).await;
        assert_eq!(s, want, "{}", path);
    }
    let ctx = f.h.ctx();
    ctx.monitor.drain_now(ctx);
    let c = ctx.logs.conn().unwrap();
    let column = |sql: &str| -> Vec<String> {
        c.prepare(sql)
            .unwrap()
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .filter_map(|r| r.unwrap())
            .collect()
    };
    let paths = column("SELECT path FROM traffic_logs");
    assert!(
        paths.iter().any(|p| p == "/flow/webhook/<redacted>"),
        "the call was logged: {:?}",
        paths
    );
    let tried = column("SELECT paths_attempted FROM error_404_tracker");
    for text in paths.iter().chain(tried.iter()) {
        for hidden in [
            token.as_str(),
            secret.as_str(),
            guessed_secret,
            "unknown-token-0123",
        ] {
            assert!(!text.contains(hidden), "{}", text);
        }
    }
}

/// R-39..R-41, R-44, R-46: the settings answers the audit named: the Host
/// Server address (trailing slash dropped) is the base; an unknown
/// workflow is 404 on GET and 500 on regenerate-secret, as the web; enable
/// answers the settings; the public address answers only POST.
#[tokio::test]
async fn the_settings_answers_follow_the_web() {
    let f = F::new();
    let id = f.create(webhook_graph("Settings")).await;
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/webhook/enable", id))
        .await;
    assert_eq!(s, StatusCode::OK);
    let token = v["webhook_token"].as_str().unwrap().to_string();
    let base = format!("http://127.0.0.1:{}", f.h.ctx().listening_port());
    assert_eq!(v["status"], "success");
    assert_eq!(v["message"], "Webhook enabled");
    assert_eq!(v["webhook_enabled"], true);
    assert_eq!(v["webhook_secret"].as_str().map(str::len), Some(64));
    assert_eq!(v["webhook_url"], format!("{}/flow/webhook/{}", base, token));
    assert_eq!(
        v["webhook_url_with_symbol"],
        format!("{}/flow/webhook/{}/{{symbol}}", base, token)
    );
    {
        let conn = f.h.ctx().sqlite.conn().unwrap();
        crate::config::save(
            &conn,
            &crate::config::ServerConfigUpdate {
                host_server: Some("https://abc.ngrok.app/".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    f.h.ctx().reload_config().unwrap();
    let v = settings(&f, id).await;
    assert_eq!(
        v["webhook_url"],
        format!("https://abc.ngrok.app/flow/webhook/{}", token)
    );
    let (s, v) = f
        .call(Method::GET, "/flow/api/workflows/999/webhook", None)
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::NOT_FOUND,
            json!({"error": "Workflow not found"})
        )
    );
    let (s, v) = f
        .post("/flow/api/workflows/999/webhook/regenerate-secret")
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to regenerate secret"})
        )
    );
    let mut get = hook(&format!("/flow/webhook/{}", token), "", None, "127.0.0.1");
    *get.method_mut() = Method::GET;
    let (s, _, _) = f.h.send(get).await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
}

/// T-070: a webhook runs a workflow whatever its trigger node (the web's
/// webhook never checks the trigger type): a manual start and an
/// order-update trigger here.
#[tokio::test]
async fn a_webhook_runs_a_workflow_whose_trigger_is_not_a_webhook_node() {
    let f = F::new();
    for (kind, data, line) in [
        ("start", json!({"scheduleType": "manual"}), None),
        (
            "orderUpdateTrigger",
            json!({"symbol": "SBIN"}),
            Some("Order-update trigger activated"),
        ),
    ] {
        let mut g = webhook_graph(kind);
        g["nodes"][0] =
            json!({"id": "w", "type": kind, "position": {"x": 0, "y": 0}, "data": data});
        let id = f.create(g).await;
        let (_, v) = f
            .post(&format!("/flow/api/workflows/{}/webhook/enable", id))
            .await;
        let (s, a) = f
            .post(&format!("/flow/api/workflows/{}/activate", id))
            .await;
        assert_eq!(s, StatusCode::OK, "{}", a);
        let body = json!({"secret": v["webhook_secret"], "symbol": "TCS", "qty": 3}).to_string();
        let (s, _, r) = f
            .send_hook(
                &format!("/flow/webhook/{}", v["webhook_token"].as_str().unwrap()),
                &body,
                "127.0.0.1",
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{}: {}", kind, r);
        let logs = f.last_run_logs(id).await;
        assert!(
            logs.contains(&"[LOG] Got TCS x3 secret={{webhook.secret}}".to_string()),
            "{}: {:?}",
            kind,
            logs
        );
        if let Some(line) = line {
            assert!(logs.contains(&line.to_string()), "{:?}", logs);
        }
    }
}

/// R-56: a payload left empty once the secret is taken out injects
/// nothing: `{{webhook}}` stays as written (an injected `{}` would read
/// `{}`), while a payload with data is the variable.
#[tokio::test]
async fn an_empty_payload_injects_nothing() {
    let f = F::new();
    let mut g = webhook_graph("Empty");
    g["nodes"][1]["data"]["message"] = json!("payload={{webhook}}");
    let id = f.create(g).await;
    let (_, v) = f
        .post(&format!("/flow/api/workflows/{}/webhook/enable", id))
        .await;
    f.post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    let url = format!("/flow/webhook/{}", v["webhook_token"].as_str().unwrap());
    let secret = v["webhook_secret"].as_str().unwrap().to_string();
    for (body, line) in [
        (json!({"secret": secret}), "[LOG] payload={{webhook}}"),
        (
            json!({"secret": secret, "qty": 2}),
            "[LOG] payload={'qty': 2}",
        ),
    ] {
        let (s, _, r) = f.send_hook(&url, &body.to_string(), "127.0.0.1").await;
        assert_eq!(s, StatusCode::OK, "{}", r);
        let logs = f.last_run_logs(id).await;
        assert!(logs.contains(&line.to_string()), "{:?}", logs);
    }
}

/// T-085: replacing an active workflow with a trigger that cannot be
/// registered stands it down and says so.
#[tokio::test]
async fn a_replace_whose_trigger_cannot_be_registered_says_so() {
    let f = F::new();
    let mut g = webhook_graph("Daily");
    g["nodes"][0] = json!({"id": "w", "type": "start", "position": {"x": 0, "y": 0},
        "data": {"scheduleType": "daily", "time": "09:15"}});
    let id = f.create(g.clone()).await;
    let (s, _) = f
        .post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    assert_eq!(s, StatusCode::OK);
    g["nodes"][0]["data"] =
        json!({"scheduleType": "once", "executeAt": "2020-01-02", "time": "09:15"});
    let (s, v) = f
        .call(
            Method::POST,
            &format!("/flow/api/workflows/{}/replace", id),
            Some(&g.to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["needs_reactivate"], true);
    assert_eq!(
        v["message"],
        "Trigger changed and could not be re-registered, so the workflow was deactivated. Activate it again when the trigger is valid."
    );
    let (_, w) = f
        .call(Method::GET, &format!("/flow/api/workflows/{}", id), None)
        .await;
    assert_eq!(w["is_active"], false);
}
