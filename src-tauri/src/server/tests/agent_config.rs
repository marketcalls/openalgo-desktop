//! The Agent's configuration routes against the web's `blueprints/agent.py`:
//! the paths and methods, the status codes, the envelopes and the shapes a
//! carried-over `/agent/config` page reads, and the credential test against
//! a fake provider (nothing leaves the machine).

use super::*;
use crate::agent::testing::{FakeProvider, Reply};
use crate::security::Unlocked;

/// Signed in, with keys to seal Agent keys under.
fn harness() -> (H, String, String) {
    let h = H::new();
    h.ctx().security.install(&Unlocked::random().unwrap());
    let (cookie, csrf) = h.session(true);
    (h, cookie, csrf)
}

fn call(
    method: Method,
    path: &str,
    body: Option<Value>,
    cookie: &str,
    csrf: &str,
) -> Request<Body> {
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    with_session(r, cookie, Some(csrf))
}

#[tokio::test]
async fn every_config_route_is_the_webs_and_needs_the_trader() {
    let table = routes::table();
    let agent: Vec<String> = table
        .iter()
        .filter(|r| r.path.starts_with("/agent/api"))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    for route in [
        "GET /agent/api/status",
        "GET /agent/api/catalog/providers",
        "GET /agent/api/catalog/models",
        "GET /agent/api/models",
        "POST /agent/api/models",
        "PATCH /agent/api/models/{id}",
        "DELETE /agent/api/models/{id}",
        "POST /agent/api/models/{id}/test",
        "POST /agent/api/models/{id}/default",
        "GET /agent/api/settings",
        "PUT /agent/api/settings",
        "GET /agent/api/kill-switch",
        "PUT /agent/api/kill-switch",
    ] {
        assert!(agent.iter().any(|r| r == route), "{} is not routed", route);
    }
    let h = H::new();
    for path in [
        "/agent/api/status",
        "/agent/api/models",
        "/agent/api/settings",
    ] {
        let (s, v) = h.json(get(path)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", path);
        assert_eq!(
            v,
            json!({"status": "error", "message": "Not authenticated"})
        );
    }
    let (cookie, _) = h.session(true);
    let no_token = with_session(
        post_json(
            "/agent/api/models",
            json!({"provider_kind": "openai", "model_name": "x"}),
        ),
        &cookie,
        None,
    );
    let (s, _) = h.json(no_token).await;
    assert!(
        !s.is_success(),
        "a write needs the session's CSRF token ({})",
        s
    );
}

#[tokio::test]
async fn the_setup_gate_and_the_catalogue_answer_as_the_web() {
    let (h, c, t) = harness();
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/status", None, &c, &t))
        .await;
    assert_eq!(s, StatusCode::OK);
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "status",
            "configured",
            "model_count",
            "default_model_id",
            "trading_enabled",
            "agent_available",
            "has_openalgo_api_key",
            "chatgpt_authorised"
        ]
    );
    assert_eq!(
        (v["configured"].clone(), v["agent_available"].clone()),
        (json!(false), json!(true))
    );
    let (s, v) = h
        .json(call(
            Method::GET,
            "/agent/api/catalog/providers",
            None,
            &c,
            &t,
        ))
        .await;
    assert_eq!((s, v["available"].clone()), (StatusCode::OK, json!(true)));
    assert!(v["data"].as_array().unwrap().len() >= 20);
    let (s, v) = h
        .json(call(
            Method::GET,
            "/agent/api/catalog/models?provider=anthropic",
            None,
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["provider"]["id"], "anthropic");
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/catalog/models", None, &c, &t))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        v,
        json!({"status": "error", "message": "A provider is required"})
    );
}

#[tokio::test]
async fn the_registry_answers_with_the_webs_codes() {
    let (h, c, t) = harness();
    let made =
        json!({"provider_kind": "openai", "model_name": "gpt-4o", "api_key": "sk-test-key-123456"});
    let (s, v) = h
        .json(call(
            Method::POST,
            "/agent/api/models",
            Some(made.clone()),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["status"], "success");
    assert!(!v.to_string().contains("sk-test"), "never the key");
    let id = v["data"]["id"].as_i64().unwrap();
    let (s, v) = h
        .json(call(Method::POST, "/agent/api/models", Some(made), &c, &t))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::CONFLICT,
            json!("gpt-4o is already registered for this provider")
        )
    );
    let (s, v) = h
        .json(call(
            Method::POST,
            "/agent/api/models",
            Some(json!({"provider_kind": "x"})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["status"], "error");
    let (s, _) = h
        .json(call(
            Method::PATCH,
            "/agent/api/models/999",
            Some(json!({"display_name": "x"})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = h
        .json(call(
            Method::PATCH,
            &format!("/agent/api/models/{}", id),
            Some(json!({})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("Nothing to update"))
    );
    let (s, v) = h
        .json(call(
            Method::POST,
            &format!("/agent/api/models/{}/default", id),
            None,
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v["message"],
        "Test the model's credentials before making it the default"
    );
    let (s, v) = h
        .json(call(Method::DELETE, "/agent/api/models/abc", None, &c, &t))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::NOT_FOUND, json!("Not found"))
    );
    let (s, v) = h
        .json(call(
            Method::DELETE,
            &format!("/agent/api/models/{}", id),
            None,
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Model deleted"})
        )
    );
    let (s, _) = h
        .json(call(
            Method::DELETE,
            &format!("/agent/api/models/{}", id),
            None,
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn settings_are_read_and_written_as_the_web() {
    let (h, c, t) = harness();
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/settings", None, &c, &t))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["system_prompt"], Value::Null);
    assert_eq!(v["defaults"]["system_prompt"], json!(""));
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/settings",
            Some(json!({})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::BAD_REQUEST, json!("Nothing to update"))
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/settings",
            Some(json!({"nope": 1})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["message"].clone()),
        (
            StatusCode::BAD_REQUEST,
            json!("Unknown agent setting(s): nope")
        )
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/settings",
            Some(json!({"trading_enabled": true})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["data"]["trading_enabled"].clone()),
        (StatusCode::OK, json!(true))
    );
    let (s, v) = h
        .json(call(
            Method::PUT,
            "/agent/api/kill-switch",
            Some(json!({"engaged": true})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(
        (s, v["data"]["engaged"].clone()),
        (StatusCode::OK, json!(true))
    );
    let (_, v) = h
        .json(call(Method::GET, "/agent/api/kill-switch", None, &c, &t))
        .await;
    assert_eq!(v["data"]["by_file"], json!(true));
    let (s, _) = h
        .json(call(
            Method::PUT,
            "/agent/api/kill-switch",
            Some(json!({"engaged": "yes"})),
            &c,
            &t,
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    h.json(call(
        Method::PUT,
        "/agent/api/kill-switch",
        Some(json!({"engaged": false})),
        &c,
        &t,
    ))
    .await;
}

/// The web's credential test: one capped streamed request, the provider's
/// own words on a failure (without the key), the first pass making the
/// model the default; 12 a minute.
#[tokio::test]
async fn a_credential_test_reaches_the_provider_once_and_records_the_result() {
    let (h, c, t) = harness();
    let fake = FakeProvider::start(vec![
        Reply::Fail(
            401,
            r#"{"error":{"message":"Incorrect API key provided: local-key-12345678.","type":"invalid_request_error"}}"#,
        ),
        Reply::Text(vec!["p"]),
    ])
    .await;
    let body = json!({"provider_kind": "openai_compatible", "model_name": "qwen",
                      "base_url": fake.endpoint(), "api_key": "local-key-12345678"});
    let (_, v) = h
        .json(call(Method::POST, "/agent/api/models", Some(body), &c, &t))
        .await;
    let id = v["data"]["id"].as_i64().unwrap();
    let path = format!("/agent/api/models/{}/test", id);
    let (s, v) = h.json(call(Method::POST, &path, None, &c, &t)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["message"], "Incorrect API key provided: [redacted].");
    assert_eq!(v["data"]["last_test_ok"], json!(false));
    let (s, v) = h.json(call(Method::POST, &path, None, &c, &t)).await;
    assert_eq!((s, v["ok"].clone()), (StatusCode::OK, json!(true)));
    assert_eq!(v["message"], "The provider accepted the request");
    assert_eq!(
        v["data"]["is_default"],
        json!(true),
        "the first pass is the default"
    );
    let sent = fake.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["max_tokens"], json!(1));
    assert_eq!(sent[1]["stream"], json!(true));
    assert_eq!(sent[1]["messages"][0]["content"], json!("ping"));
    let (s, v) = h
        .json(call(Method::GET, "/agent/api/status", None, &c, &t))
        .await;
    assert_eq!((s, v["configured"].clone()), (StatusCode::OK, json!(true)));
    // Twelve tests a minute, then the web's JSON 429.
    let mut limited = None;
    for _ in 0..12 {
        let (s, v) = h.json(call(Method::POST, &path, None, &c, &t)).await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(v);
            break;
        }
    }
    let v = limited.expect("the 13th test in a minute is refused");
    assert_eq!(
        v["message"],
        "Rate limit exceeded. Please slow down your requests."
    );
}
